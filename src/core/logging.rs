use std::cmp::PartialEq;

use std::fs::File;

use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::path::PathBuf;

use thiserror::Error;

use crate::core::platform::linux::LinuxAsyncLines;
use crate::core::task::TaskError;

use crate::threads::AsyncTask;
use crate::threads::TaskSender;

#[cfg(target_os = "linux")]
const DEFAULT_LOG_DIR_LOCATION: &'static str = "/var/log/yapm/";
#[cfg(target_os = "windows")]
static DEFAULT_LOG_DIR_LOCATION: &'static str = "";

//all logging by each process would go to a seperate file
//two buffers will exist with one connected to all the stdouts
//and another to the stderr

//we might also need to register this as a proper log manager; for processes that do not output to stdout/err
//the first thing we need is a thread-safe buffer
//the next thing we need is means of tracking where each logfile is
//another thing is to cap max-buffer size, and also to cap the logfile length with active truncation
//so maybe we need a struct to represent the state of a process and it's file. So process-name/pid and the path to it's stdout and stderr

#[derive(Debug, Error)]
pub enum LoggingError {
    #[error("An important assertion has failed: {}", .msg)]
    AssertionError { msg: String },
    #[error("An I/O error occured: {}", .0)]
    IOError(#[from] std::io::Error),
    #[error("{}", .0)]
    TaskError(#[from] TaskError),
}

type LogOpResult<T> = Result<T, LoggingError>;

#[derive(Debug, PartialEq)]
pub enum FMaxSize {
    Infinite,
    Capped(usize), //in kbs is reasonable
}

//do we not need to give the handles to the processlogger to manage?
//so that all of that can be managed here
#[derive(Debug)]
pub struct ProcessLogger {
    stderr: Option<File>,
    stdout: Option<File>,
    stderr_src: Option<LinuxAsyncLines>,
    stdout_src: Option<LinuxAsyncLines>,
    stderr_size: u64, //get this with file metadata
    stdout_size: u64,
    fmax_size: FMaxSize,
    pid: i32, //incase we need to do any weird stuff with the linux ABI
}

impl ProcessLogger {
    //define a truncate policy later
    pub fn truncate_file(max_len: u64, curr_len: u64, fhandle: &mut File) -> LogOpResult<()> {
        let value = max_len / 4;
        if curr_len <= max_len {
            return Ok(()); //should be the most common operation
        }
        fhandle.seek(std::io::SeekFrom::Start(curr_len - value))?;
        let mut b = Vec::with_capacity(value as usize);
        fhandle.read_to_end(&mut b)?;
        fhandle.set_len(0)?;
        fhandle.seek(std::io::SeekFrom::Start(0))?;
        fhandle.write_all(&b)?;
        Ok(())
    }

    fn new(
        pid: i32,
        pname: String,
        fstdoutpath: Option<PathBuf>,
        fstderrpath: Option<PathBuf>,
    ) -> LogOpResult<Self> {
        let (fhandleout, fhandlerr) =
            if let (Some(outpath), Some(errpath)) = (fstdoutpath, fstderrpath) {
                if outpath == errpath {
                    return Err(LoggingError::AssertionError {
                        msg: format!("{:?} should not be equal to {:?}", outpath, errpath),
                    });
                } else {
                    let outhandle = File::options()
                        .create(true)
                        .read(true)
                        .write(true)
                        .append(true)
                        .open(outpath)?;
                    let errhandle = File::options()
                        .create(true)
                        .read(true)
                        .write(true)
                        .append(true)
                        .open(errpath)?;
                    (outhandle, errhandle)
                }
            } else {
                let root = PathBuf::from(DEFAULT_LOG_DIR_LOCATION);
                let stderr_path = PathBuf::from(format!("{}_err.log", pname));
                let stdout_path = PathBuf::from(format!("{}_output.log", pname));
                let root_stderr = root.join(&stderr_path);
                let root_stdout = root.join(&stdout_path);
                #[cfg(debug_assertions)]
                {
                    dbg!(&root_stderr);
                    dbg!(&root_stdout);
                }
                let outhandle = File::options()
                    .create(true)
                    .read(true)
                    .write(true)
                    .append(true)
                    .open(root_stdout)?;
                let errhandle = File::options()
                    .create(true)
                    .read(true)
                    .write(true)
                    .append(true)
                    .open(root_stderr)?;
                (outhandle, errhandle)
            };

        let err_size = fhandlerr.metadata()?.len();
        let out_size = fhandleout.metadata()?.len();

        let max_fsize = FMaxSize::Capped(4096);

        return Ok(Self {
            stderr: Some(fhandlerr),
            stdout: Some(fhandleout),
            fmax_size: max_fsize,
            stderr_src: None,
            stdout_src: None,
            stderr_size: err_size,
            stdout_size: out_size,
            pid,
        });
    }

    pub fn set_handles(&mut self, stderr: LinuxAsyncLines, stdout: LinuxAsyncLines) {
        self.stderr_src = Some(stderr);
        self.stdout_src = Some(stdout);
    }

    pub fn send_streaming_task(&mut self, sender: TaskSender) -> LogOpResult<()> {
        if self.stderr_src.is_none()
            || self.stdout_src.is_none()
            || self.stdout.is_none()
            || self.stderr.is_none()
        {
            return Err(LoggingError::AssertionError {
                msg: String::from("There is no valid I/O source"),
            });
        }

        //we literally just made the check above though, so yeah
        let ferr = self.stderr.take().expect("");
        let fout = self.stdout.take().expect("");
        let serr = self.stderr_src.take().expect("");
        let sout = self.stdout_src.take().expect("");
        let max_size = if let FMaxSize::Capped(cap) = self.fmax_size {
            cap
        } else {
            usize::MAX
        };

        match sender.send(AsyncTask::StreamLogTask(
            max_size,
            self.stderr_size,
            self.stdout_size,
            serr,
            sout,
            ferr,
            fout,
            self.pid,
        )) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("Attempt to send task info while async runtime is unavailable");
                return Err(LoggingError::AssertionError {
                    msg: String::from("Attempt to send task info while runtime is unavailable"),
                });
            }
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_logger_init() {
        //just make sure it doesn't panic
        let first = ProcessLogger::new(102, String::from("test_one"), None, None);
        assert!(true)
    }
}
