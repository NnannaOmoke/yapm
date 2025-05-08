use crate::core::logging::ProcessLogger;
use crate::core::platform::linux::LinuxAsyncLines;

use std::collections::HashMap;
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

use tokio::runtime::Builder;
use tokio::runtime::Runtime;

use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Mutex as AsyncMutex;

use tokio::task::JoinHandle;

pub type TaskSender = UnboundedSender<AsyncTask>;

#[derive(Debug)]
pub enum AsyncTask {
    StreamIOToFile(LinuxAsyncLines, LinuxAsyncLines, File, File, i32),
    //error size, out size, error, error, ferr, fout, pid
    StreamLogTask(
        usize,
        u64,
        u64,
        LinuxAsyncLines,
        LinuxAsyncLines,
        File,
        File,
        i32,
    ),
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
    #[error("The task was not found in the async registry")]
    TaskNotFound,
    #[error("Inconsistent runtime state")]
    InconsistentRuntimeState,
    #[error("Sending the task to the runtime failed: {}", .msg)]
    TaskSendError { msg: String },
}

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for AsyncError {
    fn from(value: tokio::sync::mpsc::error::SendError<T>) -> Self {
        Self::TaskSendError {
            msg: value.to_string(),
        }
    }
}

type AsyncResult<T> = Result<T, AsyncError>;
type TaskId = usize;
type Pid = u32;

pub struct LoggingTask {
    max_size: usize,
    err_size: usize,
    out_size: usize,
    ierr: LinuxAsyncLines,
    iout: LinuxAsyncLines,
    ferr: File,
    fout: File,
    pid: Pid,
}

pub struct MetricsTask {
    pid: Pid,
    task_arc: Arc<AsyncMutex<()>>,
}

pub struct ReviveTask {
    pid: Pid,
    task_arc: Arc<Mutex<()>>,
}

#[derive(Clone, Debug)]
pub enum TaskStatus {
    Running,
    Completed,
    Failed(String),
}

#[derive(Clone, Debug)]
pub enum TaskType {
    Logging,
    Metrics,
    Process,
}

#[derive(Debug)]
pub struct TaskInfo {
    id: TaskId,
    pid: Pid,
    ty: TaskType,
    handles: Vec<JoinHandle<()>>,
    status: TaskStatus,
    created: OffsetDateTime,
    desc: String,
}

struct TaskRegistry {
    tasks: HashMap<TaskId, TaskInfo>,
    pid_index: HashMap<Pid, Vec<TaskId>>,
    next: TaskId,
}

impl TaskRegistry {
    fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            pid_index: HashMap::new(),
            next: 1,
        }
    }

    fn allocate(&mut self) -> TaskId {
        let id = self.next;
        self.next += 1;
        id
    }

    fn register_task(&mut self, id: TaskId, pid: Pid, tty: TaskType, desc: String) {
        let info = TaskInfo {
            id,
            pid,
            ty: tty,
            handles: vec![],
            status: TaskStatus::Running,
            created: OffsetDateTime::now_utc(),
            desc,
        };
        self.tasks.insert(id, info);
        self.pid_index.entry(pid).or_insert_with(Vec::new).push(id);
    }

    fn add_handle(&mut self, id: TaskId, handle: JoinHandle<()>) -> AsyncResult<()> {
        if let Some(task) = self.tasks.get_mut(&id) {
            task.handles.push(handle);
            Ok(())
        } else {
            Err(AsyncError::TaskNotFound)
        }
    }

    fn update_status(&mut self, id: TaskId, status: TaskStatus) -> AsyncResult<()> {
        if let Some(task) = self.tasks.get_mut(&id) {
            task.status = status;
            Ok(())
        } else {
            Err(AsyncError::TaskNotFound)
        }
    }

    fn get_task(&self, id: TaskId) -> Option<&TaskInfo> {
        self.tasks.get(&id)
    }

    fn get_task_mut(&mut self, id: TaskId) -> Option<&mut TaskInfo> {
        self.tasks.get_mut(&id)
    }

    fn get_tasks_by_pid(&self, pid: Pid) -> Vec<&TaskInfo> {
        if let Some(ids) = self.pid_index.get(&pid) {
            ids.iter().filter_map(|id| self.tasks.get(id)).collect()
        } else {
            vec![]
        }
    }

    fn remove_task(&mut self, id: TaskId) -> AsyncResult<TaskInfo> {
        match self.tasks.remove(&id) {
            Some(task) => {
                if self.pid_index.remove(&task.pid).is_none() {
                    return Err(AsyncError::InconsistentRuntimeState);
                }
                Ok(task)
            }
            None => Err(AsyncError::TaskNotFound),
        }
    }
}

pub struct GlobalAsyncIOManager {
    rt: Runtime,
    sender: UnboundedSender<AsyncTask>,
    state: RuntimeState,
}

pub struct TGlobalAsyncIOManager {
    rt: Runtime,

    log_sender: UnboundedSender<(TaskId, LoggingTask)>,
    metrics_sender: UnboundedSender<(TaskId, MetricsTask)>,
    process_sender: UnboundedSender<(TaskId, ReviveTask)>,

    registry: Arc<AsyncMutex<TaskRegistry>>,
    log_queue: Arc<AsyncMutex<VecDeque<String>>>,
    state: RuntimeState,
}

impl TGlobalAsyncIOManager {
    pub fn new() -> AsyncResult<Self> {
        let (ltx, lrx) = unbounded_channel();
        let (mtx, mrx) = unbounded_channel();
        let (ptx, prx) = unbounded_channel();
        let rt = Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("YAPM async handler")
            .build()?;
        let registry = Arc::new(AsyncMutex::new(TaskRegistry::new()));
        let lqueue = Arc::new(AsyncMutex::new(VecDeque::with_capacity(20)));

        let mut manager = Self {
            rt,
            log_sender: ltx,
            metrics_sender: mtx,
            process_sender: ptx,
            registry,
            log_queue: lqueue,
            state: RuntimeState::Unavailable,
        };
        manager.start_runtime(lrx, mrx, prx);
        Ok(manager)
    }

    fn start_runtime(
        &mut self,
        lrx: UnboundedReceiver<(TaskId, LoggingTask)>,
        mtx: UnboundedReceiver<(TaskId, MetricsTask)>,
        rtx: UnboundedReceiver<(TaskId, ReviveTask)>,
    ) {
    }

    fn is_runtime_up(&self) -> bool {
        matches!(self.state, RuntimeState::Running)
    }

    pub async fn submit_logging_task(&self, task: LoggingTask, desc: &str) -> AsyncResult<TaskId> {
        if !self.is_runtime_up() {
            return Err(AsyncError::UnitializedRuntimeError);
        }

        let pid = task.pid;
        let mut registry = self.registry.lock().await;
        let id = registry.allocate();
        registry.register_task(id, pid, TaskType::Logging, desc.to_string());
        self.log_sender.send((id, task))?;
        Ok(id)
    }

    pub async fn submit_metrics_task(&self, task: MetricsTask, desc: &str) -> AsyncResult<TaskId> {
        if !self.is_runtime_up() {
            return Err(AsyncError::UnitializedRuntimeError);
        }
        let pid = task.pid;
        let mut registry = self.registry.lock().await;
        let id = registry.allocate();
        registry.register_task(id, pid, TaskType::Metrics, desc.to_string());
        Ok(id)
    }

    pub async fn submit_process_monitoring_task(
        &self,
        task: ReviveTask,
        desc: &str,
    ) -> AsyncResult<TaskId> {
        if !self.is_runtime_up() {
            return Err(AsyncError::UnitializedRuntimeError);
        }
        let pid = task.pid;
        let mut registry = self.registry.lock().await;
        let id = registry.allocate();
        registry.register_task(id, pid, TaskType::Metrics, desc.to_string());
        Ok(id)
    }

    pub async fn kill_task(&self, task_id: TaskId) -> AsyncResult<()> {
        let mut registry = self.registry.lock().await;
        let task = registry
            .get_task_mut(task_id)
            .ok_or(AsyncError::TaskNotFound)?;
        for handle in task.handles.iter_mut() {
            handle.abort()
        }
        registry.update_status(task_id, TaskStatus::Completed)?;
        Ok(())
    }

    pub async fn kill_tasks_by_pid(&self, pid: Pid) -> AsyncResult<()> {
        let mut registry = self.registry.lock().await;
        let tids = if let Some(tids) = registry.pid_index.get(&pid) {
            tids.clone()
        } else {
            return Ok(());
        };

        for id in tids {
            if let Some(task) = registry.get_task_mut(id) {
                for h in task.handles.iter_mut() {
                    h.abort()
                }
                task.status = TaskStatus::Completed;
            }
        }

        Ok(())
    }

    pub async fn get_task_statuses(&self) -> Vec<(TaskId, Pid, TaskType, TaskStatus, String)> {
        let registry = self.registry.lock().await;
        registry
            .tasks
            .values()
            .map(|t| (t.id, t.pid, t.ty.clone(), t.status.clone(), t.desc.clone()))
            .collect()
    }

    pub async fn get_task_status(&self, task_id: TaskId) -> AsyncResult<TaskStatus> {
        let registry = self.registry.lock().await;
        registry
            .get_task(task_id)
            .map(|t| t.status.clone())
            .ok_or(AsyncError::TaskNotFound)
    }

    pub async fn get_task_ids_by_pid(&self, pid: Pid) -> Vec<TaskId> {
        let registry = self.registry.lock().await;
        registry
            .get_tasks_by_pid(pid)
            .iter()
            .map(|task| task.id)
            .collect()
    }
}

enum RuntimeState {
    Running,
    Unavailable,
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

    pub fn give_sender(&self) -> AsyncResult<TaskSender> {
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
                AsyncTask::StreamLogTask(
                    size_max,
                    size_err,
                    size_out,
                    stderr,
                    stdout,
                    ferr,
                    fout,
                    pid,
                ) => {
                    let another = glv.clone();
                    let handle_err = tokio::spawn(async move {
                        Self::truncate_stream_stderr(
                            stderr,
                            ferr,
                            glv_clone,
                            size_max as u64,
                            size_err as u64,
                        )
                        .await
                    });
                    let handle_out = tokio::spawn(async move {
                        Self::truncate_stream_stdout(
                            stdout,
                            fout,
                            another,
                            size_max as u64,
                            size_err as u64,
                        )
                        .await
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

    //this doesn't kill the process; it just collects the handle
    pub fn kill_async(handle: JoinHandle<AsyncResult<()>>) {
        handle.abort();
    }

    pub async fn stream_stderr(
        stderr: LinuxAsyncLines,
        ferr: File,
        glv: Arc<Mutex<VecDeque<String>>>,
        pid: i32,
    ) -> AsyncResult<()> {
        stream_to_file(stderr, ferr, glv, true).await
    }

    pub async fn stream_stdout(
        stdout: LinuxAsyncLines,
        ferr: File,
        glv: Arc<Mutex<VecDeque<String>>>,
        pid: i32,
    ) -> AsyncResult<()> {
        stream_to_file(stdout, ferr, glv, false).await
    }

    pub async fn truncate_stream_stderr(
        stderr: LinuxAsyncLines,
        ferr: File,
        glv: Arc<Mutex<VecDeque<String>>>,
        max_size: u64,
        curr_size: u64,
    ) -> AsyncResult<()> {
        truncate_stream_to_file(max_size, curr_size, stderr, ferr, glv, true).await?;
        Ok(())
    }

    pub async fn truncate_stream_stdout(
        stdout: LinuxAsyncLines,
        fout: File,
        glv: Arc<Mutex<VecDeque<String>>>,
        max_size: u64,
        curr_size: u64,
    ) -> AsyncResult<()> {
        truncate_stream_to_file(max_size, curr_size, stdout, fout, glv, false).await?;
        Ok(())
    }

    pub fn drop_runtime(mut self) {
        *(&mut self.state) = RuntimeState::Unavailable;
        //should block for about a second
        self.rt.shutdown_timeout(Duration::from_secs(1));
    }
}

pub async fn stream_to_file(
    mut source: LinuxAsyncLines,
    mut target: File,
    inter: Arc<Mutex<VecDeque<String>>>,
    tstream: bool,
) -> AsyncResult<()> {
    let tstream = if tstream { "OutStream" } else { "ErrStream" };
    while let Some(line) = source.next_line().await? {
        let time = OffsetDateTime::now_local()?;
        let complete = format! {
            "{}: [{}:{}:{}, {}:{}:{}] {}\n",
            tstream,
            time.hour(),
            time.minute(),
            time.second(),
            time.day(),
            time.month(),
            time.year(),
            line
        };
        target.write(complete.as_bytes())?;
        target.flush()?;
        let curr = inter.clone();
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

pub async fn truncate_stream_to_file(
    max_size: u64,
    mut curr_size: u64,
    mut source: LinuxAsyncLines,
    mut target: File,
    inter: Arc<Mutex<VecDeque<String>>>,
    tstream: bool,
) -> AsyncResult<()> {
    let tstream = if tstream { "OutStream" } else { "ErrStream" };
    while let Some(line) = source.next_line().await? {
        let time = OffsetDateTime::now_local()?;
        let complete = format! {
            "{}: [{}:{}:{}, {}:{}:{}] {}\n",
            tstream,
            time.hour(),
            time.minute(),
            time.second(),
            time.day(),
            time.month(),
            time.year(),
            line
        };
        target.write(complete.as_bytes())?;
        target.flush()?;
        curr_size += complete.len() as u64;
        let curr = inter.clone();
        let lock = curr.lock();
        if let Ok(mut lk) = lock {
            if lk.len() >= 10 {
                lk.pop_front();
                lk.push_back(complete);
            } else {
                lk.push_back(complete);
            }
        }
        //TODO: appropriate error conversion here
        ProcessLogger::truncate_file(max_size, curr_size, &mut target).expect("");
    }
    Ok(())
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
