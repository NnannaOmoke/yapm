use tokio::sync::mpsc::UnboundedReceiver;

use nix::unistd::Pid;

use nix::libc::sysconf;
use nix::libc::_SC_CLK_TCK;

use std::ops::Deref;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use tokio::sync::mpsc::UnboundedSender;

use dashmap::DashMap;

use crate::protocol::CommandResult;
use crate::threads::ReviveTask;
use crate::{config::ProcessConfig, threads::AsyncRuntime};

use super::logging::LogEntry;
use super::logging::LogManager;
use super::logging::LogType;
use super::task::LinuxCurrentTask;

pub struct RestartRequest {
    pub name: String,
}

#[derive(Debug)]
pub struct ProcessRegistry {
    pub tasks: DashMap<String, LinuxCurrentTask>, //are we sure we need a mutex for this? I don't think we do
}

impl ProcessRegistry {
    pub fn new() -> Self {
        Self {
            tasks: DashMap::new(),
        }
    }
}

pub struct ProcessManager {
    registry: Arc<ProcessRegistry>,
    pub runtime: Arc<AsyncRuntime>,
    logger: Arc<StdMutex<LogManager>>,
    restart_sender: UnboundedSender<RestartRequest>,
}

impl ProcessManager {
    //start the global context
    pub fn init() -> Result<Self, CommandResult> {
        let registry = Arc::new(ProcessRegistry::new());
        let logger = Arc::new(StdMutex::new(LogManager::new()));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let runtime = match AsyncRuntime::new(tx.clone(), logger.clone(), registry.clone()) {
            Ok(rt) => Arc::new(rt),
            Err(e) => {
                return Err(CommandResult::Error(format!(
                    "We could not initialize the runtime: Error: {e}"
                )))
            }
        };
        //runtime should have started right there; we'll then proceed to spawn the restart handler after
        let manager = Self {
            registry: registry.clone(),
            runtime: runtime.clone(),
            logger: logger.clone(),
            restart_sender: tx,
        };
        Self::start_restart_handler(rx, runtime, registry, logger);
        Ok(manager)
    }

    pub async fn start(&self, cfg: ProcessConfig) -> CommandResult {
        if let Some(value) = self.registry.tasks.get(&cfg.name) {
            return CommandResult::Error(format!(
                "A process tagged with name: {} exists",
                &cfg.name
            ));
        }
        let rt = self.runtime.clone();
        let mut cmd = match unsafe {
            crate::core::platform::linux::create_child_exec_context_from_config(cfg.clone())
        } {
            Ok(cmd) => cmd,
            Err(e) => {
                return CommandResult::Error(format!("Spawning the child process failed: {e}"))
            }
        };
        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        let pid = Pid::from_raw(cmd.pid());
        let metrics = match crate::core::platform::linux::LinuxRuntimeMetrics::new(pid) {
            Ok(metrics) => metrics,
            Err(e) => {
                let _ = cmd.sigkill();
                return CommandResult::Error(format!(
                    "Could not create metrics for this task: {e}"
                ));
            }
        };
        let name = cfg.name.clone();
        let mut logger =
            match crate::core::logging::ProcessLogger::new(cmd.pid(), name.clone(), None, None) {
                Ok(logger) => logger,
                Err(e) => {
                    let _ = cmd.sigkill();
                    return CommandResult::Error(format!(
                        "Could not start logger for this task: {e}"
                    ));
                }
            };
        let (out, err) = cmd.readers();
        logger.set_handles(err, out);
        match logger.send_to_runtime(rt.deref()).await {
            Ok(_) => {}
            Err(e) => {
                let _ = cmd.sigkill();
                return CommandResult::Error(format!(
                    "Could not register streaming task with runtime: {e}"
                ));
            }
        };
        let mut entry = LinuxCurrentTask {
            config: cfg,
            inner: cmd,
            metrics,
        };
        match rt
            .submit_process_monitoring_task(
                ReviveTask {
                    pid,
                    name: name.clone(),
                },
                &format!("Registering lifecycle task, pid: {pid}"),
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                let _ = entry.inner.sigkill();
                let _ = self.runtime.kill_tasks_by_pid(pid);
                return CommandResult::Error(format!(
                    "Could not register lifetime task with runtime: {e}"
                ));
            }
        };
        match rt
            .submit_metrics_task(
                crate::threads::Metrics {
                    pid,
                    name: name.clone(),
                },
                format!("Registering metrics task, pid: {pid}"),
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                let _ = entry.inner.sigkill();
                let _ = self.runtime.kill_tasks_by_pid(pid);
                return CommandResult::Error(format!(
                    "Could not register lifetime task with runtime: {e}"
                ));
            }
        };
        self.registry.tasks.insert(name.clone(), entry);
        CommandResult::ProcessStarted {
            name,
            pid: pid.as_raw(),
        }
    }

    pub async fn stop(&self, process: String) -> CommandResult {
        let (k, mut v) = match self.registry.tasks.remove(&process) {
            Some((k, v)) => (k, v),
            None => {
                return CommandResult::Error(format!(
                    "The specified process: {} is not managed by YAPM",
                    &process
                ))
            }
        };
        let pid = Pid::from_raw(v.inner.pid());
        match self.runtime.kill_tasks_by_pid(pid) {
            Ok(_) => {}
            Err(e) => {
                return CommandResult::Error(format!(
                    "YAPM's runtime could not terminate asynchronous tasks. Please restart YAPM"
                ))
            }
        }
        //if this doesn't work, this means you have a zombie, and you might have to kill it yourself
        match v.inner.sigkill() {
            Ok(_) => {}
            Err(e) => {
                return CommandResult::Error(format!(
                    "YAPM could not terminate the process with pid: {pid}"
                ))
            }
        }
        CommandResult::ProcessStopped { name: process }
    }

    pub async fn list_processes(&self) -> CommandResult {
        let mut container = vec![];
        let keys: Vec<String> = self
            .registry
            .tasks
            .iter()
            .map(|r| r.key().clone())
            .collect();

        for name in keys {
            if let Some(ref_val) = self.registry.tasks.get(&name) {
                let valref = ref_val.value();
                let pid = valref.inner.pid();
                let rtime = std::time::Instant::now() - valref.inner.start;
                let rtime = rtime.as_secs_f32();
                let display = time::Duration::seconds_f32(rtime);
                let mem = valref.metrics.vmrss_kb;
                let cpu_ticks = valref.metrics.uptime + valref.metrics.children_uptime;
                let ctics_per_second = unsafe { sysconf(_SC_CLK_TCK) };
                let rate = cpu_ticks as f32 / ctics_per_second as f32;
                let percent = (rate as f32 / rtime) * 100.0;

                let info = crate::protocol::ProcessInfo {
                    name: name.clone(),
                    pid,
                    runtime: format!(
                        "{}hrs{}mins",
                        display.whole_hours(),
                        display.whole_minutes()
                    ),
                    cpu_usage: percent,
                    memory_usage: mem,
                };
                container.push(info);
            }
        }

        CommandResult::ProcessList { list: container }
    }

    pub async fn get_process_details(&self, target: String) -> CommandResult {
        let g = match self.registry.tasks.get(&target) {
            Some(v) => v,
            None => {
                return CommandResult::Error(format!(
                    "The process: {} is not managed by YAPM",
                    &target
                ))
            }
        };
        let details = crate::protocol::ProcessDetails {
            name: target.clone(),
            pid: g.inner.pid(),
            vmrss_kb: g.metrics.vmrss_kb,
            vmswap_kb: g.metrics.vmswap_kb,
            rssanon_kb: g.metrics.rssanon_kb,
            rssfile_kb: g.metrics.rssfile_kb,
            rssshmem_kb: g.metrics.rssshmem_kb,
            uptime: g.metrics.uptime,
            kernel_time: g.metrics.kernel_time,
            nthreads: g.metrics.nthreads,
            read_bytes: g.metrics.read_bytes,
            write_bytes: g.metrics.write_bytes,
            args: g.config.args.clone(),
            cwd: g.config.cwd.clone(),
            env: g.config.env.clone(),
            restart: g.config.restart.clone(),
            resources: g.config.resources.clone(),
            security: g.config.security.clone(),
        };
        return CommandResult::ProcessDetails(details);
    }

    pub async fn stop_all(&self) -> CommandResult {
        let mut failures = vec![];
        match self.runtime.kill_runtime().await {
            Ok(_) => {}
            Err(_) => {} //it doesn't really matter, the only execution path that could lead here terminates the whole process anyways
        };
        self.registry.tasks.iter_mut().for_each(|mut task| {
            let pid = task.inner.pid();
            if let Err(e) = task.inner.sigkill() {
                failures.push(format!("Failed to kill process with pid: {pid}: {e}"));
            }
        });
        if failures.is_empty() {
            CommandResult::Success("All tasks have been terminated".to_string())
        } else {
            CommandResult::Error(failures.join("; "))
        }
    }

    fn start_restart_handler(
        mut rx: UnboundedReceiver<RestartRequest>,
        runtime: Arc<AsyncRuntime>,
        registry: Arc<ProcessRegistry>,
        logger: Arc<StdMutex<LogManager>>,
    ) {
        let run_clone = runtime.clone();
        runtime.rt.spawn(async move {
            while let Some(RestartRequest { name }) = rx.recv().await {
                let (name, mut details) = registry.tasks.remove(&name).unwrap();
                let _ = run_clone
                    .kill_tasks_by_pid(Pid::from_raw(details.inner.pid()));
                let decision = details.respawn();
                if !decision {
                    drop(details);
                    let mut g = logger.lock().unwrap();
                    g.push(LogEntry::new(
                        crate::core::logging::LogType::YapmLog,
                        format!("Process with tag: {name} has been removed from the YAPM runtime"),
                    ));
                    drop(g);
                    registry.tasks.remove(&name);
                } else {
                    let rt = run_clone.clone();
                    let config = details.config.clone();
                    let mut cmd = match unsafe {
                        crate::core::platform::linux::create_child_exec_context_from_config(
                            config.clone(),
                        )
                    } {
                        Ok(cmd) => cmd,
                        Err(e) => {
                            let mut g = logger.lock().unwrap();
                            g.push(LogEntry::new(
                                LogType::YapmWarning,
                                format!("Could not restart process with tag: {name}"),
                            ));
                            return;
                        }
                    };
                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    let pid = Pid::from_raw(cmd.pid());
                    let metrics = match crate::core::platform::linux::LinuxRuntimeMetrics::new(pid)
                    {
                        Ok(metrics) => metrics,
                        Err(e) => {
                            let mut g = logger.lock().unwrap();
                            g.push(LogEntry::new(
                                LogType::YapmWarning,
                                format!("Could not create process metrics for tag: {name}"),
                            ));
                            let _ = cmd.sigkill();
                            return;
                        }
                    };
                    let mut log = match crate::core::logging::ProcessLogger::new(
                        cmd.pid(),
                        name.clone(),
                        None,
                        None,
                    ) {
                        Ok(logger) => logger,
                        Err(e) => {
                            let mut g = logger.lock().unwrap();
                            g.push(LogEntry::new(
                                LogType::YapmWarning,
                                format!("Could not create process logger for tag: {name}"),
                            ));
                            let _ = cmd.sigkill();
                            return;
                        }
                    };
                    let (out, err) = cmd.readers();
                    log.set_handles(err, out);
                    match log.send_to_runtime(rt.deref()).await {
                        Ok(_) => {}
                        Err(e) => {
                            let mut g = logger.lock().unwrap();
                            g.push(LogEntry::new(
                                LogType::YapmWarning,
                                format!(
                                    "Could not create re-register process logger for tag: {name}"
                                ),
                            ));
                            let _ = cmd.sigkill();
                            return;
                        }
                    };
                    let mut entry = LinuxCurrentTask {
                        config,
                        inner: cmd,
                        metrics,
                    };
                    match rt
                        .submit_process_monitoring_task(
                            ReviveTask {
                                pid,
                                name: name.clone(),
                            },
                            &format!("Registering lifecycle task, pid: {pid}"),
                        )
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            let mut g = logger.lock().unwrap();
                            g.push(LogEntry::new(
                                LogType::YapmWarning,
                                format!(
                                    "Could not create re-register process lifecycle task for tag: {name}"
                                ),
                            ));
                            rt.kill_tasks_by_pid(pid);
                            let _ = entry.inner.sigkill();
                            return;
                        }
                    };
                    match rt
                        .submit_metrics_task(
                            crate::threads::Metrics {
                                pid,
                                name: name.clone(),
                            },
                            format!("Registering metrics task, pid: {pid}"),
                        )
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            let mut g = logger.lock().unwrap();
                            g.push(LogEntry::new(
                                LogType::YapmWarning,
                                format!(
                                    "Could not create re-register process lifecycle metrics for tag: {name}"
                                ),
                            ));
                            rt.kill_tasks_by_pid(pid);
                            let _ = entry.inner.sigkill();
                            return;
                        }
                    };
                    registry.tasks.insert(name.clone(), entry);
                }
            }
        });
    }

    pub fn get_logs(&self, n: usize) -> Vec<LogEntry> {
        let g = self.logger.lock().unwrap();
        g.glv.iter().rev().take(n).map(|t| t.clone()).collect()
    }

    pub fn log(&self, entry: LogEntry) {
        let mut g = self.logger.lock().unwrap();
        g.push(entry);
        drop(g);
    }
}
