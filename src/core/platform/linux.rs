use nix::errno::Errno;

use nix::unistd::Pid;

use nix::sys::signal::kill;
use nix::sys::signal::SIGKILL;

use thiserror::Error;

type LinuxOpResult<T> = Result<T, LinuxErrorManager>;

#[derive(Debug, Error)]
pub enum LinuxErrorManager {
    #[error("SIGKILL could not terminate the running process; YAPM does not have the neccessary permissions to terminate the child process")]
    SigKillNoPerm,
    #[error(
        "SIGKILL could not terminate the target process; It either does not exist or has been terminated already"
    )]
    SigKillNoExist,
    #[error("There has been an unknown error: {}", .0)]
    UnexpectedError(Errno),
}

impl From<Errno> for LinuxErrorManager {
    fn from(err: Errno) -> Self {
        match err {
            Errno::EPERM => LinuxErrorManager::SigKillNoPerm,
            Errno::ESRCH => LinuxErrorManager::SigKillNoExist,
            err_code => LinuxErrorManager::UnexpectedError(err_code),
        }
    }
}

pub fn kill_process_sigkill(id: i32) -> LinuxOpResult<()> {
    kill(Pid::from_raw(id), SIGKILL)?;
    Ok(())
}
