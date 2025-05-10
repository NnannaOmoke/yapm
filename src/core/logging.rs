use std::cmp::PartialEq;

use std::fs::File;

use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::path::PathBuf;

use circular_buffer::CircularBuffer;

use termcolor::ColorSpec;

use termcolor::WriteColor;
use thiserror::Error;
use time::error::IndeterminateOffset;
use time::OffsetDateTime;

use crate::core::platform::linux::LinuxAsyncLines;
use crate::core::platform::linux::Pid;
use crate::core::task::TaskError;
use crate::threads::LoggingTask;
use crate::threads::TGlobalAsyncIOManager;

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
    #[error("Time init error: {}", .0)]
    TimeError(#[from] IndeterminateOffset),
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
    pid: Pid, //incase we need to do any weird stuff with the linux ABI
}

impl ProcessLogger {
    //define a truncate policy later
    pub fn truncate_file(max_len: usize, curr_len: usize, fhandle: &mut File) -> LogOpResult<()> {
        let value = max_len / 4;
        if curr_len <= max_len {
            return Ok(()); //should be the most common operation
        }
        fhandle.seek(std::io::SeekFrom::Start((curr_len - value) as u64))?;
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
            pid: Pid::from_raw(pid),
        });
    }

    pub fn set_handles(&mut self, stderr: LinuxAsyncLines, stdout: LinuxAsyncLines) {
        self.stderr_src = Some(stderr);
        self.stdout_src = Some(stdout);
    }

    pub fn send_streaming_task(
        &mut self,
        sender: impl std::ops::Deref<Target = TGlobalAsyncIOManager>,
    ) -> LogOpResult<()> {
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
        //TODO: ifx thhis
        let _ = sender.submit_logging_task(
            LoggingTask {
                max_size,
                err_size: self.stderr_size as usize,
                out_size: self.stdout_size as usize,
                ierr: serr,
                iout: sout,
                ferr,
                fout,
                pid: self.pid,
            },
            "Registering Log task",
        );

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogType {
    Stdout,
    Stderr,
    YapmErr,
    YapmLog,
    YapmWarning,
}

impl LogType {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogType::Stdout => "Outstream",
            LogType::Stderr => "Errstream",
            LogType::YapmErr => "Yapm-Err",
            LogType::YapmLog => "Yapm-Log",
            LogType::YapmWarning => "Yapm-Warn",
        }
    }

    pub fn colour(&self) -> ColorSpec {
        let mut spec = ColorSpec::new();
        match self {
            LogType::Stdout => {
                spec.set_fg(Some(termcolor::Color::White));
            }
            LogType::Stderr => {
                //trying to approximate Orange for terminals
                spec.set_fg(Some(termcolor::Color::Ansi256(214)));
            }
            LogType::YapmErr => {
                spec.set_fg(Some(termcolor::Color::Red))
                    .set_bold(true)
                    .set_intense(true);
            }
            LogType::YapmLog => {
                spec.set_fg(Some(termcolor::Color::Blue));
            }
            LogType::YapmWarning => {
                spec.set_fg(Some(termcolor::Color::Yellow))
                    .set_intense(true);
            }
        }
        spec
    }
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    ty: LogType,
    timestamp: OffsetDateTime,
    msg: String,
}

impl LogEntry {
    pub fn new(ty: LogType, msg: String) -> Self {
        Self {
            ty,
            timestamp: OffsetDateTime::now_local().unwrap_or(OffsetDateTime::now_utc()),
            msg,
        }
    }

    pub fn to_fstring(&self) -> String {
        format!(
            "{} [{}:{:02}:{:02}, {}-{}-{}] {}\n",
            self.ty.as_str(),
            self.timestamp.hour(),
            self.timestamp.minute(),
            self.timestamp.second(),
            self.timestamp.day(),
            self.timestamp.month(),
            self.timestamp.year(),
            self.msg
        )
    }
}

impl std::fmt::Display for LogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [{}:{:02}:{:02}, {}-{}-{}] {}\n",
            self.ty.as_str(),
            self.timestamp.hour(),
            self.timestamp.minute(),
            self.timestamp.second(),
            self.timestamp.day(),
            self.timestamp.month(),
            self.timestamp.year(),
            self.msg
        )
    }
}

#[derive(Debug, Clone)]
pub struct LogManager {
    glv: CircularBuffer<100, LogEntry>,
}

impl LogManager {
    pub fn new() -> Self {
        let buf = CircularBuffer::new();
        Self { glv: buf }
    }

    pub fn push(&mut self, entry: LogEntry) {
        self.glv.push_back(entry);
    }

    pub fn len(&self) -> usize {
        self.glv.len()
    }

    //display to the terminal
    pub fn display(&self, mut n: usize) -> LogOpResult<()> {
        if n > 100 {
            n = 100;
        }
        let mut stream = termcolor::BufferedStandardStream::stderr(termcolor::ColorChoice::Auto);
        for e in self.glv.iter().rev().take(n).rev() {
            let code = e.ty.colour();
            stream.set_color(&code)?;
            stream.write(e.to_fstring().as_bytes())?;
            stream.set_color(&termcolor::ColorSpec::new())?;
            stream.flush()?;
        }
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

    #[test]
    fn test_log_entry_formatting() {
        let time = time::OffsetDateTime::now_local().unwrap();
        let entry = LogEntry {
            ty: LogType::Stdout,
            // timestamp: datetime!(2025-05-09 12:34:56 UTC),
            timestamp: time,
            msg: "Hello, World!".into(),
        };

        let string = format!(
            "{} [{}:{:02}:{:02}, {}-{}-{}] {}\n",
            "Outstream",
            time.hour(),
            time.minute(),
            time.second(),
            time.day(),
            time.month(),
            time.year(),
            "Hello, World!"
        );
        assert_eq!(entry.to_fstring(), string);
    }
    #[test]
    fn test_log_manager_push_and_display() {
        let mut mgr = LogManager::new();

        let entry1 = LogEntry::new(LogType::Stdout, "Entry 1".to_string());
        let entry2 = LogEntry::new(LogType::Stderr, "Entry 2".to_string());
        let entry3 = LogEntry::new(LogType::YapmErr, "Error!".to_string());
        let entry4 = LogEntry::new(LogType::YapmLog, "Log!".to_string());
        let entry5 = LogEntry::new(LogType::YapmWarning, "Warning".to_string());
        mgr.push(entry1);
        mgr.push(entry2);
        mgr.push(entry3);
        mgr.push(entry4);
        mgr.push(entry5);

        mgr.display(5).unwrap();
    }
}
