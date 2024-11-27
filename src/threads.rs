use std::collections::VecDeque;

use std::fmt::Debug;

use std::fs::File;

use std::io::Write;

use std::process::ChildStderr;
use std::process::ChildStdout;

use std::sync::Arc;
use std::sync::Mutex;

use std::time::Duration;

use thiserror::Error;

use time::error::IndeterminateOffset;
use time::OffsetDateTime;

use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;

use tokio::process::ChildStderr as TokioStderr;
use tokio::process::ChildStdout as TokioStdout;

use tokio::runtime::Builder;
use tokio::runtime::Runtime;

use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

use tokio::task::JoinHandle;

pub enum AsyncTask {
    StreamIOToFile(ChildStderr, ChildStdout, File, File, i32),
    KillThread(i32),
}

#[derive(Debug, Error)]
pub enum AsyncError {
    #[error("Unknown Error")]
    UnknownAsyncError,
    #[error("Uninitialized Runtime")]
    UnitializedRuntimeError,
    #[error("I/O error: {}", .0)]
    AysncIOError(#[from] std::io::Error),
    #[error("Time init error: {}", .0)]
    TimeError(#[from] IndeterminateOffset),
}

enum RuntimeState {
    Running,
    Unavailable,
}

type AsyncResult<T> = Result<T, AsyncError>;

pub struct GlobalAysncIOManager {
    rt: Runtime,
    sender: UnboundedSender<AsyncTask>,
    state: RuntimeState,
}

impl GlobalAysncIOManager {
    pub fn new(glv: Arc<Mutex<VecDeque<String>>>) -> AsyncResult<Self> {
        let (tx, rx) = unbounded_channel();
        let rt = Builder::new_current_thread()
            .worker_threads(4)
            .thread_name("YAPM async handler")
            .build()?;
        rt.spawn(Self::coordinator(rx, glv));
        Ok(Self {
            sender: tx,
            rt,
            state: RuntimeState::Running,
        })
    }

    pub fn give_sender(&self) -> AsyncResult<UnboundedSender<AsyncTask>> {
        if let RuntimeState::Running = self.state {
            Ok(self.sender.clone())
        } else {
            Err(AsyncError::UnitializedRuntimeError)
        }
    }

    pub async fn coordinator(
        mut rx: UnboundedReceiver<AsyncTask>,
        glv: Arc<Mutex<VecDeque<String>>>,
    ) {
        let mut tasks = vec![];
        while let Some(msg) = rx.recv().await {
            let glv_clone = glv.clone();
            match msg {
                AsyncTask::StreamIOToFile(stderr, stdout, ferr, fout, pid) => {
                    let handle = tokio::spawn(async move {
                        Self::stream_to_file(stderr, stdout, ferr, fout, glv_clone, pid).await
                    });
                    tasks.push((pid, handle));
                }
                AsyncTask::KillThread(pid) => {
                    //get the task, handle pair, collect it and then remove it
                    if let Some(pos) = tasks.iter().position(|&(ipid, _)| ipid == pid) {
                        let handle = tasks.remove(pos).1;
                        Self::kill_async(handle);
                    }
                }
            }
        }
    }

    pub fn kill_async(handle: JoinHandle<AsyncResult<()>>) {
        handle.abort();
    }

    pub async fn stream_to_file(
        stderr: ChildStderr,
        stdout: ChildStdout,
        mut ferr: File,
        mut fout: File,
        global_log_vector: Arc<Mutex<VecDeque<String>>>,
        pid: i32,
    ) -> AsyncResult<()> {
        let stderr = TokioStderr::from_std(stderr)?;
        let stdout = TokioStdout::from_std(stdout)?;

        let mut err_reader = BufReader::new(stderr).lines();
        let mut out_reader = BufReader::new(stdout).lines();

        while let Some(line) = err_reader.next_line().await? {
            //we need to get the time crate so that we can format this properly
            let time = OffsetDateTime::now_local()?;
            let complete = format! {"[{}:{}:{}:{}:{}:{}] {}\n", time.year(), time.month(), time.day(), time.hour(), time.minute(), time.millisecond(), line};
            ferr.write(complete.as_bytes())?;
            ferr.flush()?;
            let curr = global_log_vector.clone();
            let lock = curr.lock();
            if let Ok(mut lk) = lock {
                if lk.len() >= 10 {
                    lk.pop_front();
                    lk.push_back(complete);
                } else {
                    lk.push_back(complete)
                }
            }
        }

        while let Some(line) = out_reader.next_line().await? {
            let time = OffsetDateTime::now_local()?;
            let complete = format! {"ErrStream: [{}:{}:{}:{}:{}:{}] {}\n", time.year(), time.month(), time.day(), time.hour(), time.minute(), time.millisecond(), line};
            fout.write(complete.as_bytes())?;
            fout.flush()?;
            let curr = global_log_vector.clone();
            let lock = curr.lock();
            if let Ok(mut lk) = lock {
                if lk.len() >= 10 {
                    lk.pop_front();
                    lk.push_back(complete);
                } else {
                    lk.push_back(complete)
                }
            }
        }

        Ok(())
    }

    pub fn drop_runtime(mut self) {
        *(&mut self.state) = RuntimeState::Unavailable;
        self.rt.shutdown_timeout(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs::File;

    use std::process::ChildStderr;
    use std::process::ChildStdout;
    use std::process::Command;
    use std::process::Stdio;

    use std::rc::Rc;
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::sync::Mutex;

    use std::thread::sleep;
    use std::thread::spawn;
    #[test]
    fn init_async_runtime() {
        let (tx, rx) = channel();
        let mut cmd = Command::new("examples/test_one.sh")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("The process could not be spawned");
        let pid = cmd.id() as i32;
        let stderr = cmd.stderr.take().unwrap();
        let stdout = cmd.stdout.take().unwrap();
        let glv = Arc::new(Mutex::new(VecDeque::new()));
        let glv_clone = glv.clone();
        let fout = File::create("examples/out.txt").unwrap();
        let ferr = File::options()
            .create(true)
            .write(true)
            .open("examples/err.txt")
            .unwrap();
        let handle = spawn(move || {
            let global_manager =
                GlobalAysncIOManager::new(glv_clone).expect("The GAM could not be initialized");
            let tx_s = global_manager
                .give_sender()
                .expect("The GAM had still not started up");
            tx.send(tx_s).unwrap();
        });
        let tx_s = rx
            .recv()
            .expect("Well something happened that we didn't expect");
        let task_m = AsyncTask::StreamIOToFile(stderr, stdout, ferr, fout, pid);
        if let Err(e) = tx_s.send(task_m) {
            panic!("{}", e);
        };
        //we sleep this thread, and then see what's up thread so we have to do all of that on another thread
        sleep(Duration::new(2, 0));
        assert!(glv.lock().unwrap().len() == 1);
        sleep(Duration::new(5, 0));
        assert!(glv.lock().unwrap().len() == 2);
    }
}
