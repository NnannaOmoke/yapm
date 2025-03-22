use std::cmp::PartialEq;

use std::fs::File;

use std::path::PathBuf;

use std::sync::Arc;
use std::sync::Mutex;

use thiserror::Error;

use crate::core::platform::linux::LinuxAsyncLines;
use crate::core::task::TaskError;

#[cfg(target_os = "linux")]
const DEFAULT_LOG_DIR_LOCATION: &'static str = "/usr/bin/yapm/logs";
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
    stderr: File,
    stdout: File,
    stderr_src: Option<LinuxAsyncLines>,
    stdout_src: Option<LinuxAsyncLines>,
    fmax_size: FMaxSize,
    pid: i32, //incase we need to do any weird stuff with the linux ABI
}

pub struct DeviceLogManager {
    global_stderr_buffer: Arc<Mutex<Vec<String>>>,
    global_stdout_buffer: Arc<Mutex<Vec<String>>>,
    loggers: Vec<ProcessLogger>,
}

impl DeviceLogManager {
    fn new() -> Self {
        let guard_stderr = Mutex::new(vec![]);
        let guard_stdout = Mutex::new(vec![]);
        let arc_stderr = Arc::new(guard_stderr);
        let arc_stdout = Arc::new(guard_stdout);
        let loggers = vec![];
        Self {
            global_stderr_buffer: arc_stderr,
            global_stdout_buffer: arc_stdout,
            loggers,
        }
    }
}

impl ProcessLogger {
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

        // let mutex_err = Mutex::new(fhandlerr);
        // let mutex_out = Mutex::new(fhandleout);

        // let arc_err = Arc::new(mutex_err);
        // let arc_out = Arc::new(mutex_out);

        let max_fsize = FMaxSize::Capped(4096);

        return Ok(Self {
            stderr: fhandlerr,
            stdout: fhandleout,
            fmax_size: max_fsize,
            stderr_src: None,
            stdout_src: None,
            pid,
        });
    }

    pub fn set_handles(&mut self, stderr: LinuxAsyncLines, stdout: LinuxAsyncLines) {
        self.stderr_src = Some(stderr);
        self.stdout_src = Some(stdout);
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
