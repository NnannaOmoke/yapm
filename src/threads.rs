use crate::core::platform::linux::LinuxAsyncLines;
use crate::core::platform::linux::ManagedStream;

use std::collections::VecDeque;

use std::fmt::Debug;

use std::fs::File;

use std::io::Write;

use std::sync::Arc;
use std::sync::Mutex;

use std::time::Duration;

use thiserror::Error;

use time::error::IndeterminateOffset;
// use time::util::local_offset::Soundness;
use time::OffsetDateTime;

use tokio::io::BufReader;
use tokio::io::Lines;

use tokio::runtime::Builder;
use tokio::runtime::Runtime;

use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

use tokio::task::JoinHandle;

#[derive(Debug)]
pub enum AsyncTask {
    StreamIOToFile(LinuxAsyncLines, LinuxAsyncLines, File, File, i32),
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

pub struct GlobalAsyncIOManager {
    rt: Runtime,
    sender: UnboundedSender<AsyncTask>,
    state: RuntimeState,
}

impl GlobalAsyncIOManager {
    pub fn new() -> AsyncResult<(Self, UnboundedReceiver<AsyncTask>)> {
        let (tx, rx) = unbounded_channel();
        let rt = Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("YAPM async handler")
            .build()?;
        Ok((
            Self {
                sender: tx,
                rt,
                state: RuntimeState::Unavailable,
            },
            rx,
        ))
    }

    //we want to start the runtime state here AFTER setting the values for the GlobalAsyncIOManager
    pub fn start_runtime(
        &mut self,
        rx: UnboundedReceiver<AsyncTask>,
        glv: Arc<Mutex<VecDeque<String>>>,
    ) {
        self.state = RuntimeState::Running;
        let handle = self.rt.spawn(Self::coordinator(rx, glv));
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
        let mut tasks = vec![]; //can we not make this global?, nah, this isn't a self.fn (double_muts?)
        while let Some(msg) = rx.recv().await {
            let glv_clone = glv.clone();
            match msg {
                AsyncTask::StreamIOToFile(stderr, stdout, ferr, fout, pid) => {
                    let another = glv.clone();
                    let handle_err = tokio::spawn(async move {
                        Self::stream_stderr(stderr, ferr, glv_clone, pid).await
                    });
                    let handle_out = tokio::spawn(async move {
                        Self::stream_stdout(stdout, fout, another, pid).await
                    });
                    tasks.push((pid, handle_err, handle_out));
                }
                AsyncTask::KillThread(pid) => {
                    //get the task, handle pair, collect it and then remove it
                    if let Some(pos) = tasks.iter().position(|&(ipid, _, _)| ipid == pid) {
                        let handle = tasks.remove(pos);
                        let err_handle = handle.1;
                        let out_handle = handle.2;
                        Self::kill_async(err_handle);
                        Self::kill_async(out_handle);
                    }
                }
            }
        }
    }

    pub fn kill_async(handle: JoinHandle<AsyncResult<()>>) {
        handle.abort();
    }

    pub async fn stream_stderr(
        mut stderr: Lines<BufReader<ManagedStream>>,
        mut ferr: File,
        glv: Arc<Mutex<VecDeque<String>>>,
        pid: i32,
    ) -> AsyncResult<()> {
        while let Some(line) = stderr.next_line().await? {
            let time = OffsetDateTime::now_local()?;
            let complete = format! {
                "ErrStream: [{}:{}:{}, {}:{}:{}] {}\n",
                time.hour(),
                time.minute(),
                time.second(),
                time.day(),
                time.month(),
                time.year(),
                line
            };
            ferr.write(complete.as_bytes())?;
            ferr.flush()?;
            let curr = glv.clone();
            let lock = curr.lock();
            if let Ok(mut lk) = lock {
                if lk.len() >= 10 {
                    lk.pop_front();
                    lk.push_back(complete);
                } else {
                    lk.push_back(complete);
                }
            }
        }
        Ok(())
    }

    pub async fn stream_stdout(
        mut stdout: Lines<BufReader<ManagedStream>>,
        mut ferr: File,
        glv: Arc<Mutex<VecDeque<String>>>,
        pid: i32,
    ) -> AsyncResult<()> {
        while let Some(line) = stdout.next_line().await? {
            let time = OffsetDateTime::now_local()?;
            let complete = format! {
                "OutStream: [{}:{}:{}, {}:{}:{}] {}\n",
                time.hour(),
                time.minute(),
                time.second(),
                time.day(),
                time.month(),
                time.year(),
                line
            };
            ferr.write(complete.as_bytes())?;
            ferr.flush()?;
            let curr = glv.clone();
            let lock = curr.lock();
            if let Ok(mut lk) = lock {
                if lk.len() >= 10 {
                    lk.pop_front();
                    lk.push_back(complete);
                } else {
                    lk.push_back(complete);
                }
            }
        }
        Ok(())
    }

    pub fn drop_runtime(mut self) {
        *(&mut self.state) = RuntimeState::Unavailable;
        //should block for about a second
        self.rt.shutdown_timeout(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use crate::core::platform::linux::create_child_exec_context;

    use super::*;

    use std::fs::File;

    use std::sync::Arc;
    use std::sync::Mutex;

    use std::thread::sleep;

    #[test]
    fn init_async_runtime() {
        let glv = Arc::new(Mutex::new(VecDeque::new()));
        let glv_clone = glv.clone();
        let fout = File::create("examples/out.txt").unwrap();
        let ferr = File::options()
            .create(true)
            .write(true)
            .open("examples/err.txt")
            .unwrap();
        let (mut global_manager, rx) =
            GlobalAsyncIOManager::new().expect("The GAM could not be initialized");
        global_manager.start_runtime(rx, glv_clone);
        let _guard = global_manager.rt.enter();
        let tx_s = global_manager
            .give_sender()
            .expect("The GAM has not started up");
        let mut cmd = unsafe {
            create_child_exec_context("examples/test_one.sh".to_string(), &vec![])
                .expect("spawning failed")
        };
        let pid = cmd.pid();
        let (lerr, lout) = cmd.readers();
        let task_m = AsyncTask::StreamIOToFile(lerr, lout, ferr, fout, pid);
        if let Err(e) = tx_s.send(task_m) {
            panic!("{}", e);
        };
        sleep(Duration::new(2, 0));
        assert!(glv.lock().unwrap().len() == 1);
        sleep(Duration::new(5, 0));
        assert!(glv.lock().unwrap().len() == 2);
    }

    #[test]
    fn test_async_io_task_kill() {
        let glv = Arc::new(Mutex::new(VecDeque::new()));
        let (mut gam, rx) = GlobalAsyncIOManager::new().expect("The gam could not be started");
        gam.start_runtime(rx, glv.clone());
        let _guard = gam.rt.enter();
        let mut cmds = vec![];
        for index in 0..=3 {
            let name = format! {"{}_sleep_command", index};
            let curr = unsafe {
                create_child_exec_context("examples/test_one.sh".to_string(), &vec![])
                    .expect("Failed to spawn process")
            };
            let ferr = File::create(format! {"examples/ferr_{}.txt", index}).unwrap();
            let fout = File::create(format! {"examples/fout_{}.txt", index}).unwrap();
            let id = curr.pid();
            cmds.push((name, curr, ferr, fout, id as i32))
        }
        //we have 4 spawned tasks
        //we'll start them, and then kill one of them before its able to write the last string to it's stderr
        //we'll then assert that the glv has 7 contents in it
        let third_tup_id = cmds[3].4;
        for tup in cmds {
            let mut cmd = tup.1;
            let (cerr, cout) = cmd.readers();
            let task = AsyncTask::StreamIOToFile(cerr, cout, tup.2, tup.3, tup.4);
            let tx_s = gam.give_sender().unwrap();
            tx_s.send(task).unwrap();
        }
        //now, we'll sleep for one second, and assert that the glv is of size 4
        sleep(Duration::new(1, 0));
        assert_eq!(glv.lock().unwrap().len(), 4);
        //now, get another sender, and try to kill one of the running tasks
        let tx_s = gam.give_sender().unwrap();
        let task = AsyncTask::KillThread(third_tup_id);
        tx_s.send(task).unwrap();
        //sleep again?
        sleep(Duration::new(5, 0));
        //everything should be done, now
        assert_eq!(glv.lock().unwrap().len(), 7); //one has been killed;
        gam.drop_runtime();
    }

    #[should_panic]
    #[test]
    fn test_async_order() {
        let (gam, rx) = GlobalAsyncIOManager::new().unwrap();
        let tx_s = gam.give_sender().unwrap(); //should panic, we've not yet started the runtime
    }
}
