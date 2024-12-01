use cgroups_rs::Cgroup;
use cgroups_rs::CpuResources;
use cgroups_rs::Resources;

use nix::errno::Errno;

use nix::unistd::Pid;

use nix::sys::resource::setrlimit;
use nix::sys::resource::Resource;
use nix::sys::resource::RLIM_INFINITY;

use nix::sys::signal::kill;
use nix::sys::signal::SIGCONT;
use nix::sys::signal::SIGKILL;
use nix::sys::signal::SIGSTOP;

use thiserror::Error;

type LinuxOpResult<T> = Result<T, LinuxErrorManager>;

#[derive(Default)]
pub enum CPUPriority {
    Max,
    VeryHigh,
    High,
    #[default]
    Normal,
    Low,
    VeryLow,
    Lowest,
}

#[derive(Default)]
pub enum MemReqs {
    #[default]
    Unlimited,
    Specific(u64),
}

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

pub fn suspend_process_sigstop(id: i32) -> LinuxOpResult<()> {
    kill(Pid::from_raw(id), SIGSTOP)?;
    Ok(())
}

pub fn start_suspended_process_sigcont(id: i32) -> LinuxOpResult<()> {
    kill(Pid::from_raw(id), SIGCONT)?;
    Ok(())
}

pub fn set_process_cpu_priority_rlimit(id: i32, cpu_priority: CPUPriority) -> LinuxOpResult<()> {
    match cpu_priority {
        //the hard limit here should be _lower_ than the soft limit
        CPUPriority::Max => setrlimit(Resource::RLIMIT_NICE, RLIM_INFINITY, RLIM_INFINITY)?,
        CPUPriority::VeryHigh => setrlimit(Resource::RLIMIT_NICE, 2, 1)?,
        CPUPriority::High => setrlimit(Resource::RLIMIT_NICE, 6, 5)?,
        CPUPriority::Normal => setrlimit(Resource::RLIMIT_NICE, 10, 9)?,
        CPUPriority::Low => setrlimit(Resource::RLIMIT_NICE, 12, 11)?,
        CPUPriority::VeryLow => setrlimit(Resource::RLIMIT_NICE, 15, 14)?,
        CPUPriority::Lowest => setrlimit(Resource::RLIMIT_NICE, 20, 18)?,
    };
    Ok(())
}

pub fn set_process_mem_max_rlimit(id: i32, mem_reqs: MemReqs) -> LinuxOpResult<()> {
    match mem_reqs {
        MemReqs::Specific(mibs) => setrlimit(
            Resource::RLIMIT_AS,
            mibs * 1024 * 1024,
            (mibs + 2) * 1024 * 1024,
        )?,
        _ => setrlimit(Resource::RLIMIT_AS, RLIM_INFINITY, RLIM_INFINITY)?,
    }
    Ok(())
}

pub fn set_process_mem_max_cgroups(
    id: i32,
    mem_reqs: MemReqs,
    resource: &mut Resources,
) -> LinuxOpResult<()> {
    match mem_reqs {
        MemReqs::Specific(mibs) => {
            resource.memory.memory_hard_limit = Some(mibs as i64 * 1024 * 1024);
        }
        _ => {}
    };
    Ok(())
}
