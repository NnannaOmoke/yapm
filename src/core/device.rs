use std::sync::Arc;
use tokio::sync::Mutex;

use dashmap::DashMap;

use crate::config::GlobalConfig;

use super::task::LinuxCurrentTask;
#[cfg(target_os = "linux")]
//represents global state of YAPM. This is the command interface, basically
pub struct Device {
    current_tasks: DashMap<String, Arc<Mutex<LinuxCurrentTask>>>,
    cfg: GlobalConfig,
}
