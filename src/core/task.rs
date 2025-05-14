use std::io::Error;

use std::sync::Arc;

use thiserror::Error;

use tokio::sync::Mutex;

use crate::config::ProcessConfig;
use crate::core::platform::linux::LinuxErrorManager;
use crate::core::platform::linux::ManagedLinuxProcess;

use super::platform::linux::LinuxRuntimeMetrics;

#[derive(Debug, Error)]
pub enum TaskError {
    #[error("The input task to execute is not in a parseable format")]
    BadInputFormat,
    #[error("There has been an I/O operations error: {}", .0)]
    InternalIOError(#[from] Error),
    #[error("The Linux syscall has failed: {}", .0)]
    InternalLinuxAPIError(#[from] LinuxErrorManager),
    #[error("A call has been made to a non-started child process")]
    ProcessStateUnitialized,
    #[error("The Error is of unknown origin")]
    UnknownProcessError,
    #[error("Failed to terminate process")]
    InvalidTermination,
}

type TaskResult<T> = Result<T, TaskError>;

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct LinuxCurrentTask {
    pub config: ProcessConfig,
    pub inner: ManagedLinuxProcess,
    pub metrics: LinuxRuntimeMetrics,
}

//this is meant to be constructed explicitly
impl LinuxCurrentTask {
    pub async fn refresh_metrics(&mut self) {
        //TODO: this is a cop-out, we want something more explicit, next time
        let _ = self.metrics.refresh();
    }

    pub async fn shutdown(reference: Arc<Mutex<Self>>) {
        let me = Arc::clone(&reference);
        let mut lock = me.lock().await;
        let _ = lock.inner.sigkill();
    }

    pub fn respawn(&mut self) -> bool {
        //check respawn policy
        match self.config.restart {
            crate::config::RestartPolicy::Capped(value) => {
                if value <= 1 {
                    false
                } else {
                    self.config.restart = crate::config::RestartPolicy::Capped(value - 1);
                    true
                }
            }
            crate::config::RestartPolicy::None => false,
            crate::config::RestartPolicy::Infinite => true,
        }
    }
}

#[cfg(test)]
mod tests {

    // #[test]
    // fn test_proper_init() {
    //     let arg_set_py = vec!["app.py".to_string()];
    //     let task = Task::new(&arg_set_py).unwrap(); //this would be for a normal executable
    //     assert_eq!(
    //         task,
    //         Task {
    //             inner: ChildWrapper::Initialized,
    //             args: Vec::new(),
    //             start: None,
    //             logger: None,
    //             task_type: TaskType::Python,
    //             executor: PYTHON_PATH.to_string()
    //         }
    //     );
    //     let arg_set_echo = vec!["echo", "Hello World", ">>", "/dev/null"]
    //         .iter()
    //         .map(|s| s.to_string())
    //         .collect::<Vec<String>>();
    //     let task_exec = Task::new(&arg_set_echo).unwrap();
    //     assert_eq!(
    //         task_exec,
    //         Task {
    //             inner: ChildWrapper::Initialized,
    //             args: vec![
    //                 "Hello World".to_string(),
    //                 ">>".to_string(),
    //                 "/dev/null".to_string()
    //             ],
    //             start: None,
    //             logger: None,
    //             task_type: TaskType::Executable,
    //             executor: "echo".to_string()
    //         }
    //     )
    // }

    // #[test]
    // #[should_panic]
    // fn test_non_proper_init() {
    //     let empty_arg_set = vec![];
    //     let task = Task::new(&empty_arg_set);
    //     task.unwrap();
    // }

    // #[tokio::test]
    // async fn test_running_exec() {
    //     let arg_set_echo = &["examples/test_one.sh".to_string()];
    //     let mut task = Task::new(arg_set_echo).unwrap();
    //     let _ = task.start();
    //     println!("The PID created: {}", task.inner.getpid().unwrap());
    //     sleep(Duration::new(1, 0));
    //     let _ = task.kill();
    //     assert!(true);
    // }
    // //test structure: create task, capture its PID, kill task, get all tasks running in the OS currently, and check
    // //if the PID is in the task-list, if it is, it's a zombie, and we've failed the test
    // //otherwise we're good
    // //god im too tired to write this today
    // #[tokio::test]
    // async fn test_running_exec_and_post_query() {
    //     let arg_set_echo = &["examples/test_one.sh".to_string()];
    //     let mut task = Task::new(arg_set_echo).unwrap();
    //     let _ = task.start();
    //     let pid = task.inner.getpid().unwrap();
    //     println!("The PID created: {}", pid);
    //     sleep(Duration::new(1, 0));
    //     task.kill().unwrap();
    //     assert!(kill(Pid::from_raw(pid), None).err() == Some(Errno::ESRCH));
    // }
}
