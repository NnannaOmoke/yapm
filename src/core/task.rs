use std::fs::File;

use std::io::Error;

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
const TYPESCRIPT_PATH: &'_ str = "$";

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
            handle.kill()?
        };
        Ok(())
    }

    fn getpid(&self) -> Option<i32> {
        if let Self::Started(handle) = self {
            return Some(handle.id() as i32); //returning i32 for compatibility with the nix API
        };
        None
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
    #[error("The executable could not be launched: {}", .0)]
    UnlaunchableExecutable(#[from] Error),
    #[error("The process could not be killed: {}", .0)]
    FailedProcessTermination(LinuxErrorManager),
    #[error("This operation has failed: Error code: {}", .0)]
    UnknownProcessError(LinuxErrorManager),
}

impl From<LinuxErrorManager> for TaskError {
    fn from(err: LinuxErrorManager) -> Self {
        match err {
            LinuxErrorManager::SigKillNoPerm | LinuxErrorManager::SigKillNoExist => {
                TaskError::FailedProcessTermination(err)
            }
            LinuxErrorManager::UnexpectedError(_) => TaskError::UnknownProcessError(err),
        }
    }
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
            let id = std::process::id();
            resolve_os_declarations!(let _ = linux::kill_process_sigkill(id as i32), None);
        } //kill the underlying process
    }
}

#[cfg(test)]
mod tests {

    use super::*;

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
}
