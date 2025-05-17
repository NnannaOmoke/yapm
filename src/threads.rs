use crate::core::device::ProcessRegistry;
use crate::core::device::RestartRequest;
use crate::Device;

use crate::core::logging::LogEntry;
use crate::core::logging::LogManager;
use crate::core::logging::LogType;
use crate::core::logging::ProcessLogger;
use crate::core::platform::linux::monitor_process_life;
use crate::core::platform::linux::LinuxAsyncLines;
use crate::core::platform::linux::Pid;
use crate::core::task::LinuxCurrentTask;

use std::collections::HashMap;

use std::fmt::Debug;

use std::fs::File;

use std::io::Write;

use std::sync::Arc;
use std::sync::Mutex;

use nix::sys::wait::WaitStatus;
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

use tracing::instrument;
use tracing::instrument::WithSubscriber;

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
    #[error("Error acquiring the mutex lock: {}", .msg)]
    MutexError { msg: String },
}

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for AsyncError {
    fn from(value: tokio::sync::mpsc::error::SendError<T>) -> Self {
        Self::TaskSendError {
            msg: value.to_string(),
        }
    }
}

impl<T> From<std::sync::PoisonError<T>> for AsyncError {
    fn from(value: std::sync::PoisonError<T>) -> Self {
        Self::MutexError {
            msg: value.to_string(),
        }
    }
}

type AsyncResult<T> = Result<T, AsyncError>;
type TaskId = usize;

#[derive(Debug)]
pub struct LoggingTask {
    pub max_size: usize,
    pub err_size: usize,
    pub out_size: usize,
    pub ierr: LinuxAsyncLines,
    pub iout: LinuxAsyncLines,
    pub ferr: File,
    pub fout: File,
    pub pid: Pid,
}

//TODO: refactor these tasks

pub struct MetricsTask {
    pub pid: Pid,
    pub task_arc: Arc<AsyncMutex<LinuxCurrentTask>>,
}

pub struct Metrics {
    pub pid: Pid,
    pub name: String,
}

pub struct ReviveTask {
    pub pid: Pid,
    pub name: String,
}

#[derive(Clone, Debug)]
pub enum TaskStatus {
    Running,
    Completed,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

impl TaskInfo {
    fn kill(&mut self) {
        for h in self.handles.iter() {
            h.abort();
        }
        self.handles = vec![]
    }
}

#[derive(Debug)]
pub struct TaskRegistry {
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

#[derive(Debug)]
pub struct AsyncRuntime {
    pub rt: Runtime,

    log_sender: UnboundedSender<(TaskId, LoggingTask)>,
    metrics_sender: UnboundedSender<(TaskId, Metrics)>,
    process_sender: UnboundedSender<(TaskId, ReviveTask)>,

    t_registry: Arc<Mutex<TaskRegistry>>,
    log_queue: Arc<Mutex<LogManager>>,

    prestart_tx: UnboundedSender<RestartRequest>,
    p_registry: Arc<ProcessRegistry>,

    state: RuntimeState,
}

impl AsyncRuntime {
    pub fn new(
        prestart_tx: UnboundedSender<RestartRequest>,
        log_manager: Arc<Mutex<LogManager>>,
        p_registry: Arc<ProcessRegistry>,
    ) -> AsyncResult<Self> {
        let (ltx, lrx) = unbounded_channel();
        let (mtx, mrx) = unbounded_channel();
        let (ptx, prx) = unbounded_channel();
        //if you don't specify the number of threads, it will use the number available to the system
        let rt = Builder::new_multi_thread()
            .enable_all()
            .thread_name("YAPM async handler")
            .build()?;

        let t_registry = Arc::new(Mutex::new(TaskRegistry::new()));

        let mut manager = AsyncRuntime {
            rt,
            log_sender: ltx,
            metrics_sender: mtx,
            process_sender: ptx,
            t_registry,
            log_queue: log_manager,
            prestart_tx,
            p_registry,
            state: RuntimeState::Unavailable,
        };
        manager.start_runtime(lrx, mrx, prx)?;
        Ok(manager)
    }

    pub fn start_runtime(
        &mut self,
        lrx: UnboundedReceiver<(TaskId, LoggingTask)>,
        mrx: UnboundedReceiver<(TaskId, Metrics)>,
        rrx: UnboundedReceiver<(TaskId, ReviveTask)>,
    ) -> AsyncResult<()> {
        let pregistry = self.p_registry.clone();
        let tregistry = self.t_registry.clone();
        let log_queue = self.log_queue.clone();
        let prestart_tx = self.prestart_tx.clone();

        self.rt.spawn(Self::logging_coordinator(
            lrx,
            tregistry.clone(),
            log_queue.clone(),
        ));

        self.rt.spawn(Self::metrics_coordinator(
            mrx,
            pregistry.clone(),
            tregistry.clone(),
            log_queue.clone(),
        ));

        self.rt.spawn(Self::revive_coordinator(
            rrx,
            tregistry,
            pregistry,
            prestart_tx,
            log_queue,
        ));

        Ok(())
    }

    fn is_runtime_up(&self) -> bool {
        matches!(self.state, RuntimeState::Running)
    }

    pub async fn submit_logging_task(
        &self,
        task: LoggingTask,
        desc: String,
    ) -> AsyncResult<TaskId> {
        let pid = task.pid;
        let mut registry = self.t_registry.lock()?;
        let id = registry.allocate();
        registry.register_task(id, pid, TaskType::Logging, desc);
        self.log_sender.send((id, task))?;
        Ok(id)
    }

    pub async fn submit_metrics_task(&self, task: Metrics, desc: String) -> AsyncResult<TaskId> {
        let pid = task.pid;
        let mut registry = self.t_registry.lock()?;
        let tasks = registry.get_tasks_by_pid(pid);
        if tasks
            .iter()
            .find(|tinfo| tinfo.ty == TaskType::Metrics)
            .is_some()
        {
            let mut g = self.log_queue.lock()?;
            g.push(LogEntry::new(LogType::YapmErr, format!("Inconsistent runtime state due to race condition; Process {} has spawned duplicate metrics collation tasks", pid)));
            drop(g); //just in case
            return Err(AsyncError::InconsistentRuntimeState);
        }
        let id = registry.allocate();
        registry.register_task(id, pid, TaskType::Metrics, desc);
        self.metrics_sender.send((id, task))?;
        Ok(id)
    }

    pub async fn submit_process_monitoring_task(
        &self,
        task: ReviveTask,
        desc: &str,
    ) -> AsyncResult<TaskId> {
        let pid = task.pid;
        let mut registry = self.t_registry.lock()?;

        let tasks = registry.get_tasks_by_pid(pid);
        if tasks
            .iter()
            .find(|tinfo| tinfo.ty == TaskType::Process)
            .is_some()
        {
            let mut g = self.log_queue.lock()?;
            g.push(LogEntry::new(LogType::YapmErr, format!("Inconsistent runtime state due to race condition; Process {} has spawned duplicate process lifetime monitoring tasks", pid)));
            drop(g); //just in case
            return Err(AsyncError::InconsistentRuntimeState);
        };
        let id = registry.allocate();
        registry.register_task(id, pid, TaskType::Process, desc.to_string());
        self.process_sender.send((id, task))?;
        Ok(id)
    }

    pub fn kill_tasks_by_pid(&self, pid: Pid) -> AsyncResult<()> {
        let mut registry = self.t_registry.lock()?;
        let tids = if let Some(tids) = registry.pid_index.get(&pid) {
            tids.clone()
        } else {
            return Ok(());
        };
        //cheap clone; this is about 4 elements
        for id in tids.into_iter() {
            if let Some(task) = registry.get_task_mut(id) {
                for h in task.handles.iter_mut() {
                    h.abort()
                }
                task.status = TaskStatus::Completed;
            }
        }
        drop(registry);
        Ok(())
    }

    pub async fn kill_runtime(&self) -> AsyncResult<()> {
        let mut g = self.t_registry.lock()?;
        for h in g.tasks.values_mut() {
            h.kill();
        }
        Ok(())
    }

    pub async fn logging_coordinator(
        mut rx: UnboundedReceiver<(TaskId, LoggingTask)>,
        registry: Arc<Mutex<TaskRegistry>>,
        logger: Arc<Mutex<LogManager>>,
    ) {
        while let Some((
            id,
            LoggingTask {
                max_size,
                err_size,
                out_size,
                ierr,
                iout,
                ferr,
                fout,
                pid,
            },
        )) = rx.recv().await
        {
            let log = logger.clone();
            let reg = registry.clone();

            let ohandle = tokio::spawn(async move {
                if let Err(e) =
                    Self::truncate_stream_stdout(iout, fout, log.clone(), max_size, out_size).await
                {
                    let mut g = reg.lock().unwrap();
                    let _ = g.update_status(
                        id,
                        TaskStatus::Failed(format!(
                            "Output streaming has failed for task-id: {}: {}",
                            id,
                            e.to_string()
                        )),
                    );
                    drop(g);
                    let mut l = log.lock().unwrap();
                    l.push(LogEntry::new(
                        LogType::YapmWarning,
                        format!(
                            "Output streaming has failed for task-id: {} - pid: {}",
                            id, pid
                        ),
                    ));
                    drop(l);
                }
            });
            let log = logger.clone();
            let reg = registry.clone();
            let ehandle = tokio::spawn(async move {
                if let Err(e) =
                    Self::truncate_stream_stderr(ierr, ferr, log.clone(), max_size, err_size).await
                {
                    tracing::debug!("An error has occured in the streaming function");
                    let mut g = reg.lock().unwrap();
                    let _ = g.update_status(
                        id,
                        TaskStatus::Failed(format!(
                            "Error streaming has failed for task-id: {}: {}",
                            id,
                            e.to_string()
                        )),
                    );
                    drop(g);
                    let mut l = log.lock().unwrap();
                    l.push(LogEntry::new(
                        LogType::YapmWarning,
                        format!(
                            "Output streaming has failed for task-id: {} - pid: {}",
                            id, pid
                        ),
                    ));
                    drop(l);
                }
            });
            let mut rlock = registry.lock().unwrap();
            let _ = rlock.add_handle(id, ehandle);
            let _ = rlock.add_handle(id, ohandle);
        }
    }

    async fn metrics_coordinator(
        mut rx: UnboundedReceiver<(TaskId, Metrics)>,
        process_registry: Arc<ProcessRegistry>,
        task_registry: Arc<Mutex<TaskRegistry>>,
        logger: Arc<Mutex<LogManager>>,
    ) {
        while let Some((id, Metrics { pid, name })) = rx.recv().await {
            let t_reg = task_registry.clone();
            let log = logger.clone();
            let p_reg = process_registry.clone();
            let handle = tokio::spawn(async move {
                let interval = tokio::time::Duration::from_secs(1);
                loop {
                    let mut g = match p_reg.tasks.get_mut(&name) {
                        Some(g) => g,
                        None => {
                            let mut log_lock = log.lock().unwrap();
                            log_lock.push(LogEntry::new(LogType::YapmErr, format!("Inconsistent runtime state, race condition detected for process: {name} while collecting metrics")));
                            let mut t_reg_lock = t_reg.lock().unwrap();
                            let _ = t_reg_lock.update_status(id, TaskStatus::Failed(format!("Inconsistent runtime state, race condition detected for process: {name} while collecting metrics")));
                            drop(t_reg_lock);
                            drop(log_lock);
                            break; //leave the task
                        }
                    };
                    match g.metrics.refresh() {
                        Ok(_) => {}
                        Err(e) => {
                            let mut log_lock = log.lock().unwrap();
                            log_lock.push(LogEntry::new(
                                LogType::YapmErr,
                                format!("Collating metrics for process name: {name} failed: {e}"),
                            ));
                            let mut t_reg_lock = t_reg.lock().unwrap();
                            let _ = t_reg_lock.update_status(
                                id,
                                TaskStatus::Failed(format!(
                                    "Collating metrics for process name: {name} failed: {e}"
                                )),
                            );
                            drop(t_reg_lock);
                            drop(log_lock);
                            drop(g);
                            break;
                        }
                    }
                    drop(g);
                }
            });
            let mut rlock = task_registry.lock().unwrap();
            let _ = rlock.add_handle(id, handle);
        }
    }

    pub async fn revive_coordinator(
        mut rx: UnboundedReceiver<(TaskId, ReviveTask)>,
        task_registry: Arc<Mutex<TaskRegistry>>,
        process_registry: Arc<ProcessRegistry>,
        tx: UnboundedSender<RestartRequest>,
        logger: Arc<Mutex<LogManager>>,
    ) {
        while let Some((id, ReviveTask { pid, name })) = rx.recv().await {
            let pid = Pid::from(pid);
            let log = logger.clone();
            let tx = tx.clone();
            let treg = task_registry.clone();
            let handle = tokio::spawn(async move {
                match monitor_process_life(pid).await {
                    Ok(status) => {
                        match status {
                            WaitStatus::Exited(pid, code) => {
                                let mut log_lock = log.lock().unwrap();
                                log_lock.push(LogEntry::new(LogType::YapmLog, format!("The process with name: {name} exited with status code: {code}")));
                            }
                            WaitStatus::Signaled(pid, sig, _) => {
                                let mut log_lock = log.lock().unwrap();
                                log_lock.push(LogEntry::new(LogType::YapmLog, format!("The process with name: {name} was terminated with signal: {sig}. ")));
                            }
                            _ => {}
                        }
                        //now we tell the overreaching `ProcessManager` that we have something to kill:
                        tx.send(RestartRequest { name }).unwrap();
                    }
                    Err(e) => {
                        let mut log_lock = log.lock().unwrap();
                        log_lock.push(LogEntry::new(
                            LogType::YapmErr,
                            format!("Managing the process lifetime has failed; The error: {e}"),
                        ));
                        let mut rlock = treg.lock().unwrap();
                        let _ = rlock.update_status(
                            id,
                            TaskStatus::Failed(format!(
                                "Managing the process lifetime has failed; The error: {e}"
                            )),
                        ); //honestly, we don't care very much about the above, so that's why we're cavalier with it
                        return; //leave the async task; it's done
                    }
                }
            });

            let mut rlock = task_registry.lock().unwrap();
            let _ = rlock.add_handle(id, handle);
        }
    }

    pub async fn truncate_stream_stderr(
        stderr: LinuxAsyncLines,
        ferr: File,
        glv: Arc<Mutex<LogManager>>,
        max_size: usize,
        curr_size: usize,
    ) -> AsyncResult<()> {
        Self::async_truncate_stream_to_file(max_size, curr_size, stderr, ferr, glv, true).await?;
        Ok(())
    }

    pub async fn truncate_stream_stdout(
        stdout: LinuxAsyncLines,
        fout: File,
        glv: Arc<Mutex<LogManager>>,
        max_size: usize,
        curr_size: usize,
    ) -> AsyncResult<()> {
        Self::async_truncate_stream_to_file(
            max_size as usize,
            curr_size as usize,
            stdout,
            fout,
            glv,
            false,
        )
        .await?;
        Ok(())
    }

    pub async fn async_truncate_stream_to_file(
        max_size: usize,
        mut curr_size: usize,
        mut source: LinuxAsyncLines,
        mut target: File,
        inter: Arc<Mutex<LogManager>>,
        tstream: bool,
    ) -> AsyncResult<()> {
        let tstream = if tstream {
            LogType::Stdout
        } else {
            LogType::Stderr
        };
        while let Some(line) = source.next_line().await? {
            tracing::debug!("We enter this loop");
            let entry = LogEntry::new(tstream, line);
            let complete = entry.to_fstring();
            target.write(complete.as_bytes())?;
            target.flush()?;
            curr_size += complete.len();
            let mut lock = inter.lock()?;
            lock.push(entry);
            //TODO: appropriate error conversion here
            ProcessLogger::truncate_file(max_size, curr_size, &mut target)
                .inspect_err(|e| panic!("{}", e.to_string()))
                .expect("");
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct TGlobalAsyncIOManager {
    pub rt: Runtime,

    log_sender: UnboundedSender<(TaskId, LoggingTask)>,
    metrics_sender: UnboundedSender<(TaskId, MetricsTask)>,
    process_sender: UnboundedSender<(TaskId, ReviveTask)>,

    registry: Arc<AsyncMutex<TaskRegistry>>,
    log_queue: Arc<AsyncMutex<LogManager>>,
    state: RuntimeState,
}

impl TGlobalAsyncIOManager {
    pub fn new(device: Arc<Device>) -> AsyncResult<Arc<Self>> {
        let (ltx, lrx) = unbounded_channel();
        let (mtx, mrx) = unbounded_channel();
        let (ptx, prx) = unbounded_channel();
        let rt = Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("YAPM async handler")
            .build()?;
        let registry = Arc::new(AsyncMutex::new(TaskRegistry::new()));
        let lqueue = Arc::new(AsyncMutex::new(LogManager::new()));

        let manager = Self {
            rt,
            log_sender: ltx,
            metrics_sender: mtx,
            process_sender: ptx,
            registry,
            log_queue: lqueue,
            state: RuntimeState::Running, //cop-out
        };

        let arc = Arc::new(manager);
        let rt = arc.clone();
        arc.start_runtime(rt, lrx, mrx, prx, device);
        Ok(arc)
    }

    fn start_runtime(
        &self,
        rmain: Arc<TGlobalAsyncIOManager>, //self referential bs, anyone?
        lrx: UnboundedReceiver<(TaskId, LoggingTask)>,
        mtx: UnboundedReceiver<(TaskId, MetricsTask)>,
        rtx: UnboundedReceiver<(TaskId, ReviveTask)>,
        device: Arc<Device>,
    ) {
        let registry = self.registry.clone();
        let log_queue = self.log_queue.clone();

        self.rt.spawn(Self::logging_coordinator(
            lrx,
            registry.clone(),
            log_queue.clone(),
        ));

        self.rt.spawn(Self::metrics_coordinator(
            mtx,
            registry.clone(),
            log_queue.clone(),
        ));

        self.rt.spawn(Self::revive_coordinator(
            rtx,
            device,
            rmain,
            log_queue.clone(),
            registry.clone(),
        ));
    }

    pub fn enter(&self) -> tokio::runtime::EnterGuard {
        self.rt.enter()
    }

    fn is_runtime_up(&self) -> bool {
        matches!(self.state, RuntimeState::Running)
    }

    #[instrument]
    pub async fn submit_logging_task(&self, task: LoggingTask, desc: &str) -> AsyncResult<TaskId> {
        if !self.is_runtime_up() {
            return Err(AsyncError::UnitializedRuntimeError);
        }

        let pid = task.pid;
        tracing::debug!("Let's get the registry lock now!");
        let mut registry = self.registry.lock().await;
        tracing::debug!("We've gotten the registry lock");
        let id = registry.allocate();
        registry.register_task(id, pid, TaskType::Logging, desc.to_string());
        tracing::debug!("We're sending the logging task over to the runtime now!");
        self.log_sender.send((id, task))?;
        Ok(id)
    }

    pub async fn submit_metrics_task(&self, task: MetricsTask, desc: &str) -> AsyncResult<TaskId> {
        if !self.is_runtime_up() {
            return Err(AsyncError::UnitializedRuntimeError);
        }
        let pid = task.pid;
        let mut registry = self.registry.lock().await;
        let tasks = registry.get_tasks_by_pid(pid);
        if tasks
            .iter()
            .find(|tinfo| tinfo.ty == TaskType::Metrics)
            .is_some()
        {
            // TODO: log and return
            return Err(AsyncError::InconsistentRuntimeState);
        };
        let id = registry.allocate();
        registry.register_task(id, pid, TaskType::Metrics, desc.to_string());
        self.metrics_sender.send((id, task))?;
        Ok(id)
    }

    //we have to perform the sanity check here, it's not enough to do it in the fn itself
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
        let tasks = registry.get_tasks_by_pid(pid);
        if tasks
            .iter()
            .find(|tinfo| tinfo.ty == TaskType::Process)
            .is_some()
        {
            // TODO: log and return
            return Err(AsyncError::InconsistentRuntimeState);
        };
        let id = registry.allocate();
        registry.register_task(id, pid, TaskType::Process, desc.to_string());
        self.process_sender.send((id, task))?;
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

    fn kill_tasks_by_pid_test(&self, pid: Pid) -> AsyncResult<()> {
        let mut registry = self.registry.blocking_lock();
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

    pub async fn kill_runtime(&self) {
        let mut g = self.registry.lock().await;
        for h in g.tasks.values_mut() {
            h.kill();
        }
        // self.state = RuntimeState::Unavailable;
    }

    #[instrument]
    async fn logging_coordinator(
        mut rx: UnboundedReceiver<(TaskId, LoggingTask)>,
        registry: Arc<AsyncMutex<TaskRegistry>>,
        glv: Arc<AsyncMutex<LogManager>>,
    ) {
        tracing::debug!("We've entered the logging co-ordinator now");
        while let Some((
            id,
            LoggingTask {
                max_size,
                err_size,
                out_size,
                ierr,
                iout,
                ferr,
                fout,
                pid,
            },
        )) = rx.recv().await
        {
            let reg = registry.clone();
            let log = glv.clone();
            tracing::debug!("We're spawning the error handle now");
            let ehandle = tokio::spawn(async move {
                if let Err(e) =
                    Self::truncate_stream_stderr(ierr, ferr, log.clone(), max_size, err_size).await
                {
                    tracing::debug!("An error has occured in the streaming function");
                    let mut g = reg.lock().await;
                    let _ = g.update_status(
                        id,
                        TaskStatus::Failed(format!(
                            "Error streaming has failed for task-id: {}: {}",
                            id,
                            e.to_string()
                        )),
                    );
                    //TODO: write something so that YAPM can log that this failed
                }
            });
            let reg = registry.clone();
            let log = glv.clone();
            let ohandle = tokio::spawn(async move {
                if let Err(e) =
                    Self::truncate_stream_stdout(iout, fout, log.clone(), max_size, out_size).await
                {
                    let mut g = reg.lock().await;
                    let _ = g.update_status(
                        id,
                        TaskStatus::Failed(format!(
                            "Error streaming has failed for task-id: {}: {}",
                            id,
                            e.to_string()
                        )),
                    );
                    //TODO: write something so that YAPM can log that this failed
                }
            });
            let mut rlock = registry.lock().await;
            let _ = rlock.add_handle(id, ehandle);
            let _ = rlock.add_handle(id, ohandle);
        }
    }

    pub async fn truncate_stream_stderr(
        stderr: LinuxAsyncLines,
        ferr: File,
        glv: Arc<AsyncMutex<LogManager>>,
        max_size: usize,
        curr_size: usize,
    ) -> AsyncResult<()> {
        async_truncate_stream_to_file(max_size, curr_size, stderr, ferr, glv, true).await?;
        Ok(())
    }

    pub async fn truncate_stream_stdout(
        stdout: LinuxAsyncLines,
        fout: File,
        glv: Arc<AsyncMutex<LogManager>>,
        max_size: usize,
        curr_size: usize,
    ) -> AsyncResult<()> {
        async_truncate_stream_to_file(
            max_size as usize,
            curr_size as usize,
            stdout,
            fout,
            glv,
            false,
        )
        .await?;
        Ok(())
    }

    pub async fn metrics_coordinator(
        mut rx: UnboundedReceiver<(TaskId, MetricsTask)>,
        registry: Arc<AsyncMutex<TaskRegistry>>,
        log_queue: Arc<AsyncMutex<LogManager>>,
    ) {
        while let Some((id, MetricsTask { pid, task_arc })) = rx.recv().await {
            let reg = registry.clone();
            let log = log_queue.clone();
            //perform a check if this task is already running for this pid in the registry
            //asynchronous, execution proceeds to the next rust-truction
            let handle = tokio::spawn(async move {
                let interval = tokio::time::Duration::from_secs(1);
                loop {
                    let mut g = task_arc.lock().await;

                    g.metrics.refresh().unwrap(); //TODO: log failure with YAPM

                    tokio::time::sleep(interval).await;
                }
            });
            let mut rlock = reg.lock().await;
            let _ = rlock.add_handle(id, handle);
        }
    }

    #[instrument]
    pub async fn revive_coordinator(
        mut rx: UnboundedReceiver<(TaskId, ReviveTask)>,
        device: Arc<Device>,
        rt: Arc<TGlobalAsyncIOManager>,
        log_queue: Arc<AsyncMutex<LogManager>>,
        registry: Arc<AsyncMutex<TaskRegistry>>,
    ) {
        while let Some((id, ReviveTask { pid, name })) = rx.recv().await {
            //we're about to do sorcery
            let pid = Pid::from(pid);
            match monitor_process_life(pid).await {
                Ok(status) => {
                    //TODO: add exit code and shit like that in a bit
                    let rt = rt.clone();
                    tracing::debug!("We try to acquire this lock");
                    let _ = rt.kill_tasks_by_pid(pid).await;
                    tracing::debug!("We have acquired the lock!");
                    let entry = LogEntry::new(
                        LogType::YapmLog,
                        "Process with name: {name} and pid: {pid} has exited".to_string(),
                    );
                    let _ = device.evaluate_restart(&name, rt).await;
                    tracing::debug!("The restart has been evaluated, and info returned");
                    continue;
                }
                Err(e) => {
                    //we also have to deal with this
                }
            };
        }
    }

    pub async fn get_logs(&self, n: usize) -> Vec<LogEntry> {
        let g = self.log_queue.lock().await;
        g.glv.iter().rev().take(n).map(|x| x.clone()).collect()
    }

    pub async fn log(&self, input: String, ty: LogType) {
        let entry = LogEntry::new(ty, input);
        let mut g = self.log_queue.lock().await;
        g.push(entry);
    }
}

#[derive(Debug)]
enum RuntimeState {
    Running,
    Unavailable,
}

#[instrument]
pub async fn async_truncate_stream_to_file(
    max_size: usize,
    mut curr_size: usize,
    mut source: LinuxAsyncLines,
    mut target: File,
    inter: Arc<AsyncMutex<LogManager>>,
    tstream: bool,
) -> AsyncResult<()> {
    let tstream = if tstream {
        LogType::Stdout
    } else {
        LogType::Stderr
    };
    while let Some(line) = source.next_line().await? {
        tracing::debug!("We enter this loop");
        let entry = LogEntry::new(tstream, line);
        let complete = entry.to_fstring();
        target.write(complete.as_bytes())?;
        target.flush()?;
        curr_size += complete.len();
        let mut lock = inter.lock().await;
        lock.push(entry);
        //TODO: appropriate error conversion here
        ProcessLogger::truncate_file(max_size, curr_size, &mut target)
            .inspect_err(|e| panic!("{}", e.to_string()))
            .expect("");
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use crate::core::platform::linux::create_child_exec_context;

    use super::*;

    use std::fs::File;

    use std::thread::sleep;
    use std::time::Duration;

    #[cfg(target_os = "windows")]
    #[test]
    fn init_async_runtime() {
        let fout = File::options()
            .create(true)
            .write(true)
            .append(true)
            .read(true)
            .open("examples/out.txt")
            .unwrap();
        let ferr = File::options()
            .create(true)
            .write(true)
            .append(true)
            .read(true)
            .open("examples/err.txt")
            .unwrap();

        let rt = TGlobalAsyncIOManager::new().expect("We could not start the runtime");
        let _guard = rt.rt.enter();
        dbg!(rt.is_runtime_up());
        let mut cmd = unsafe {
            create_child_exec_context("examples/test_one.sh".to_string(), &vec![])
                .expect("spawning failed")
        };

        let pid = cmd.pid();
        let (lout, lerr) = cmd.readers();
        let task = rt
            .submit_logging_task(
                LoggingTask {
                    max_size: 100,
                    err_size: 0,
                    out_size: 0,
                    ierr: lerr,
                    iout: lout,
                    ferr,
                    fout,
                    pid: Pid::from_raw(pid),
                },
                "Task spawned or some shit",
            )
            .expect("This failed");

        sleep(Duration::new(2, 0));
        let g = rt.log_queue.blocking_lock();
        assert!(g.len() == 1);
        drop(g);
        sleep(Duration::new(5, 0));
        let g = rt.log_queue.blocking_lock();
        assert!(g.len() == 2);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_async_io_task_kill_t() {
        let rt = TGlobalAsyncIOManager::new().expect("The GAM could not be entered");
        let _guard = rt.rt.enter();
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
        let third_tup_id = cmds[3].4;
        for tup in cmds {
            let mut cmd = tup.1;
            let (cout, cerr) = cmd.readers();
            let task = LoggingTask {
                max_size: 10000,
                err_size: 0,
                out_size: 0,
                ierr: cerr,
                iout: cout,
                ferr: tup.2,
                fout: tup.3,
                pid: Pid::from_raw(tup.4),
            };
            rt.submit_logging_task(task, "Log some stuff for each process")
                .expect("task sending broke");
        }

        sleep(Duration::new(1, 0));
        let g = rt.log_queue.blocking_lock();
        assert_eq!(g.len(), 4);
        drop(g);
        rt.kill_tasks_by_pid_test(Pid::from_raw(third_tup_id))
            .expect("Well, that failed");
        dbg!("we got here!");

        sleep(Duration::new(5, 0));
        let g = rt.log_queue.blocking_lock();
        assert_eq!(g.len(), 7);
    }
}
