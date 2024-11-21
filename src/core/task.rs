use std::fs::File;

use std::error::Error as MajorError;

use std::io::Error;
use std::io::Read;

use std::time::Instant;

use std::process::id;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;

use thiserror::Error;

use crate::core::platform::linux;
use crate::core::platform::linux::LinuxErrorManager;
use crate::resolve_os_declarations;

const PYTHON_PATH: &'static str = "$PYTHON";
const JAVASCRIPT_PATH: &'static str = "$";
const TYPESCRIPT_PATH: &'static str = "$";

#[derive(Debug, PartialEq)]
pub enum TaskType {
    Executable,
    Python,
    Javascript,
    Typescript,
}

#[derive(Debug)]
pub enum ChildWrapper {
    Started(Child),
    Initialized,
}

impl ChildWrapper {
    fn new(executor: &String, args: &[String]) -> TaskResult<(Self, Instant)> {
        let time = Instant::now();
        let command = Command::new(executor)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok((ChildWrapper::Started(command), time))
    }

    fn kill(&mut self) -> TaskResult<()> {
        if let Self::Started(handle) = self {
            handle.kill()?;
            //we'll have to use ASYNC for this or massive multithreading
            handle.wait()?;
        };
        Ok(())
    }

    fn getpid(&self) -> Option<i32> {
        if let Self::Started(handle) = self {
            return Some(handle.id() as i32); //returning i32 for compatibility with the nix API
        };
        None
    }
    //TODO: read stderr and stdin to a writable (this blocks?)
    #[cfg(debug_assertions)]
    fn read_to_string(&mut self, s: &mut String) -> Result<(), Box<dyn MajorError>> {
        if let Self::Started(handle) = self {
            let _ = handle.stderr.take().unwrap().read_to_string(s)?; //will block, do it async-ly
        }
        Ok(())
    }
}

//add task permissions:
//task permissions should deal with the following for now:
//- network access
//- file i/o (everything in UNIX is a file, so perhaps we want to do something about that)
#[derive(Debug)]
pub struct Task {
    inner: ChildWrapper,
    //these contain the args passed to the executor, which in some cases is a language rt (python, js) or a binary, upon
    //which the arguments passed would be arguments passed to the executor (we could even use this as a recompilation directive)
    args: Vec<String>,
    //the executor
    executor: String,
    //when the task is started
    start: Option<Instant>,
    //log-file
    file: Option<File>,
    //the task type
    task_type: TaskType,
}

#[derive(Debug, Error)]
pub enum TaskError {
    #[error("The input task to execute is not in a parseable format")]
    BadInputFormat,
    #[error("There has been an I/O operations error: {}", .0)]
    InternalIOError(#[from] Error),
    #[error("The Linux syscall has failed: {}", .0)]
    InternalLinuxAPIError(#[from] LinuxErrorManager),
    #[error("The Error is of unknown origin")]
    UnknownProcessError,
}

type TaskResult<T> = Result<T, TaskError>;

//hopefully does not compile in the release binary
#[cfg(debug_assertions)]
impl PartialEq for Task {
    fn eq(&self, other: &Task) -> bool {
        self.args == other.args
            && self.executor == other.executor
            && self.task_type == other.task_type
    }
}

impl Task {
    //for VM based applications/scripts, we need to find out the application type by splitting on the file-extension and running it with
    //its accompanying interpreter
    //if it's not VM based (i.e. a program binary, we'll need to launch the program as is)
    fn new(input: &[String]) -> TaskResult<Self> {
        let source = &input.get(0);
        if source.is_none() {
            return Err(TaskError::BadInputFormat);
        }
        let source = source.unwrap();
        let ext = source.split(".");
        let args = Vec::from(&input[1..]);
        let consumed = ext.collect::<Vec<&str>>();
        let task_type = match *consumed.last().unwrap_or(&"") {
            //always some cursed magic with rust, honestly :D
            "py" => TaskType::Python,
            "js" => TaskType::Javascript,
            "ts" => TaskType::Typescript,
            _ => TaskType::Executable, //we cannot parse the input string, so we run as executable
        };
        //default directives to execute the process
        let executor = match task_type {
            TaskType::Executable => String::from(source),
            TaskType::Python => String::from(PYTHON_PATH),
            TaskType::Javascript => String::from(JAVASCRIPT_PATH),
            TaskType::Typescript => String::from(TYPESCRIPT_PATH),
        };
        let log_file = None;
        let inner = ChildWrapper::Initialized;
        let start = None; //we'll overwrite this later when we start the process, no need to rewrite it twice
        Ok(Self {
            inner,
            args,
            start,
            file: log_file,
            task_type,
            executor,
        })
    }
    //the goal of this function is to start the child process
    //this is extremely crucial that this is able to occur
    //we will start with very simple stuff, and test extensively as we go on
    fn start(&mut self) -> TaskResult<()> {
        let (fprocess_handle, start) = ChildWrapper::new(&self.executor, &self.args)?;
        self.start = Some(start);
        self.inner = fprocess_handle;
        Ok(())
    }
    //we want this to be called by the drop implementation;if it doesn't work we'll use the linux API to kill it ...
    //by fire, by force
    fn kill(&mut self) -> TaskResult<()> {
        if let Err(e) = self.inner.kill() {
            let pid = self.inner.getpid().unwrap();
            resolve_os_declarations!(linux::kill_process_sigkill(pid)?, None);
        }
        Ok(())
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if let Err(e) = self.kill() {
            //the process still no gree die - we crash the program and have the OS SIGKILL the parent process
            //we cannot allow zombies at all
            let id = id();
            resolve_os_declarations!(let _ = linux::kill_process_sigkill(id as i32), None);
        } //kill the underlying process
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::thread::sleep;

    use std::time::Duration;

    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    #[test]
    fn test_proper_init() {
        let arg_set_py = vec!["app.py".to_string()];
        let task = Task::new(&arg_set_py).unwrap(); //this would be for a normal executable
        assert_eq!(
            task,
            Task {
                inner: ChildWrapper::Initialized,
                args: Vec::new(),
                start: None,
                file: None,
                task_type: TaskType::Python,
                executor: PYTHON_PATH.to_string()
            }
        );
        let arg_set_echo = vec!["echo", "Hello World", ">>", "/dev/null"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<String>>();
        let task_exec = Task::new(&arg_set_echo).unwrap();
        assert_eq!(
            task_exec,
            Task {
                inner: ChildWrapper::Initialized,
                args: vec![
                    "Hello World".to_string(),
                    ">>".to_string(),
                    "/dev/null".to_string()
                ],
                start: None,
                file: None,
                task_type: TaskType::Executable,
                executor: "echo".to_string()
            }
        )
    }

    #[test]
    #[should_panic]
    fn test_non_proper_init() {
        let empty_arg_set = vec![];
        let task = Task::new(&empty_arg_set);
        task.unwrap();
    }

    #[test]
    fn test_running_exec() {
        let arg_set_echo = &["examples/test_one.sh".to_string()];
        let mut task = Task::new(arg_set_echo).unwrap();
        let _ = task.start();
        println!("The PID created: {}", task.inner.getpid().unwrap());
        sleep(Duration::new(1, 0));
        let _ = task.kill();
        assert!(true);
    }
    //test structure: create task, capture its PID, kill task, get all tasks running in the OS currently, and check
    //if the PID is in the task-list, if it is, it's a zombie, and we've failed the test
    //otherwise we're good
    //god im too tired to write this today
    #[test]
    fn test_running_exec_and_post_query() {
        let arg_set_echo = &["examples/test_one.sh".to_string()];
        let mut task = Task::new(arg_set_echo).unwrap();
        let _ = task.start();
        let pid = task.inner.getpid().unwrap();
        println!("The PID created: {}", pid);
        sleep(Duration::new(1, 0));
        task.kill().unwrap();
        assert!(kill(Pid::from_raw(pid), None).err() == Some(Errno::ESRCH));
    }

    #[test]
    fn test_captured_io() {
        let arg_set_echo = &["examples/test_one.sh".to_string()];
        let mut task = Task::new(arg_set_echo).unwrap();
        let mut s = String::new();
        let _ = task.start();
        task.inner.read_to_string(&mut s).unwrap();
        sleep(Duration::new(1, 0));
        task.kill().unwrap();
        //we can capture stdout/stderr, but it's blocking
        //to fix that, we need it to be non-blocking and wrapped in an async block
        assert_eq!(s, String::from("hello, i'm running\nhello, i'm dying\n"));
    }
}
