use std::fs::File;

use std::io::Error;

use std::time::Instant;

use std::process::Child;
use std::process::Command;
use std::process::Stdio;

use thiserror::Error;

const PYTHON_PATH: &'static str = "$PYTHON";
const JAVASCRIPT_PATH: &'static str = "$";
const TYPESCRIPT_PATH: &'_ str = "$";

pub enum TaskType {
    Executable,
    Python,
    Javascript,
    Typescript,
}

pub enum ChildWrapper {
    Started(Child),
    Initialized,
}

impl ChildWrapper {
    fn new(
        name: String,
        tasktype: TaskType,
        executor: String,
        args: Vec<String>,
    ) -> TaskResult<Self> {
        let command = Command::new(executor)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(ChildWrapper::Started(command))
    }
}

//add task permissions:
//task permissions should deal with the following for now:
//- network access
//- file i/o (everything in UNIX is a file, so perhaps we want to do something about that)
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
    #[error("The executable could not be launched")]
    UnlaunchableExecutable(#[from] Error),
}

type TaskResult<T> = Result<T, TaskError>;

impl Task {
    //for VM based applications/scripts, we need to find out the application type by splitting on the file-extension and running it with
    //its accompanying interpreter
    //if it's not VM based (i.e. a program binary, we'll need to launch the program as is)
    fn new(input: Vec<String>) -> TaskResult<Self> {
        let source = &input[0];
        let ext = source.split(".");
        let args = Vec::from(&input[1..]);
        let consumed = ext.collect::<Vec<&str>>();
        let task_type = if consumed.len() > 1 {
            //then it's an extensioned file, we match on it, otherwise we assume it's a binary
            match *consumed.last().unwrap_or(&"") {
                //always some cursed magic with rust, honestly :D
                "py" => TaskType::Python,
                "js" => TaskType::Javascript,
                "ts" => TaskType::Typescript,
                _ => return Err(TaskError::BadInputFormat), //we cannot parse the input string, i.e we don't know how it's supposed to run
            }
        } else {
            TaskType::Executable
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
    //this si
    fn start(&mut self) -> TaskResult<()> {
        Ok(())
    }
}
