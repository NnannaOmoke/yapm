use nix::unistd::Pid;
use std::sync::Arc;
use tokio::sync::Mutex;

use dashmap::{DashMap, Map};

use crate::protocol::CommandResult;
use crate::test_block;
use crate::threads::ReviveTask;
use crate::{config::ProcessConfig, threads::TGlobalAsyncIOManager};

use super::task::LinuxCurrentTask;
#[cfg(target_os = "linux")]
//represents global state of YAPM. This is the command interface, basically
pub struct Device {
    current_tasks: DashMap<String, Arc<Mutex<LinuxCurrentTask>>>,
}

impl Device {
    //intialize device state
    pub fn new() -> Self {
        Self {
            current_tasks: DashMap::new(),
        }
    }

    pub async fn start(&self, cfg: ProcessConfig, rt: &TGlobalAsyncIOManager) -> CommandResult {
        if let Some(value) = self.current_tasks.get(&cfg.name) {
            return CommandResult::Error(format!(
                "A process tagged with name: {} exists",
                &cfg.name
            ));
        }
        let mut cmd = match unsafe {
            crate::core::platform::linux::create_child_exec_context_from_config(cfg.clone())
        } {
            Ok(cmd) => cmd,
            Err(e) => {
                return CommandResult::Error(format!("Spawning the child process failed: {e}"))
            }
        };
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let pid = Pid::from_raw(cmd.pid());
        let metrics = match crate::core::platform::linux::LinuxRuntimeMetrics::new(pid) {
            Ok(metrics) => metrics,
            Err(e) => {
                return CommandResult::Error(format!("Could not create metrics for this task: {e}"))
            }
        };
        let name = cfg.name.clone();
        let mut logger =
            match crate::core::logging::ProcessLogger::new(cmd.pid(), name.clone(), None, None) {
                Ok(logger) => logger,
                Err(e) => {
                    return CommandResult::Error(format!(
                        "Could not start logger for this task: {e}"
                    ))
                }
            };
        let (out, err) = cmd.readers();
        logger.set_handles(err, out);
        match logger.send_streaming_task(rt) {
            Ok(_) => {}
            Err(e) => {
                return CommandResult::Error(format!(
                    "Could not register streaming task with runtime: {e}"
                ))
            }
        };
        let entry = LinuxCurrentTask {
            config: cfg,
            inner: cmd,
            metrics,
        };
        let entry = std::sync::Arc::new(Mutex::new(entry));

        match rt
            .submit_process_monitoring_task(
                ReviveTask {
                    pid,
                    task_arc: entry.clone(),
                },
                &format!("Registering lifecycle task, pid: {pid}"),
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                return CommandResult::Error(format!(
                    "Could not register lifetime task with runtime: {e}"
                ))
            }
        };

        match rt
            .submit_metrics_task(
                crate::threads::MetricsTask {
                    pid,
                    task_arc: entry.clone(),
                },
                &format!("Registering metrics task, pid: {pid}"),
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                return CommandResult::Error(format!(
                    "Could not register lifetime task with runtime: {e}"
                ))
            }
        };
        self.current_tasks.insert(name.clone(), entry);
        CommandResult::ProcessStarted {
            name,
            pid: pid.as_raw(),
        }
    }

    pub async fn stop(&self, process: String, rt: &TGlobalAsyncIOManager) -> CommandResult {
        let (k, v) = match self.current_tasks.remove(&process) {
            Some((k, v)) => (k, v),
            None => {
                return CommandResult::Error(format!(
                    "The specified process: {} is not managed by YAPM",
                    &process
                ))
            }
        };
        let g = v.lock().await;
        let pid = Pid::from_raw(g.inner.pid());
        drop(g); //prevent deadlock
        match rt.kill_tasks_by_pid(pid).await {
            Ok(_) => {}
            Err(e) => {
                return CommandResult::Error(format!(
                    "YAPM's runtime could not terminate asynchronous tasks. Please restart YAPM"
                ))
            }
        };
        let mut g = v.lock().await;
        match g.inner.sigkill() {
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
        for t in self.current_tasks.iter() {
            let name = t.key().clone();
            let valref = t.value().lock().await;
            let pid = valref.inner.pid();
            let rtime = valref.inner.start;
            let rtime = std::time::Instant::now() - rtime;
            let rtime = time::Duration::seconds(rtime.as_secs() as i64);
            let cputime = valref.metrics.uptime;
            let mem = valref.metrics.vmrss_kb; //rss
            let info = crate::protocol::ProcessInfo {
                name,
                pid,
                runtime: format!("{}hrs{}mins", rtime.whole_hours(), rtime.whole_minutes()),
                cpu_usage: 0.0,
                memory_usage: mem,
            };
            container.push(info);
        }
        return CommandResult::ProcessList { list: container };
    }

    pub async fn get_process_details(&self, target: String) -> CommandResult {
        let v = match self.current_tasks.get(&target) {
            Some(v) => v,
            None => {
                return CommandResult::Error(format!(
                    "The process: {} is not managed by YAPM",
                    &target
                ))
            }
        };
        let g = v.value().lock().await;
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

        let arcs: Vec<_> = self
            .current_tasks
            .iter()
            .map(|entry| entry.value().clone())
            .collect();

        for task_arc in arcs {
            let mut task = task_arc.lock().await;
            let pid = task.inner.pid();
            if let Err(e) = task.inner.sigkill() {
                failures.push(format!("Failed to kill process with pid {pid}: {e}"));
            }
        }

        if failures.is_empty() {
            CommandResult::Success("All tasks have been terminated".to_string())
        } else {
            CommandResult::Error(failures.join("; "))
        }
    }
}
