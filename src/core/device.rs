use std::process::Child;
use std::process::ChildStderr;
use std::process::ChildStdin;
use std::process::ChildStdout;

use std::sync::Arc;
use std::sync::RwLock;

struct Device {
    current_tasks: Vec<Arc<RwLock<Child>>>,
}
