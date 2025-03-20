use std::ffi::CString;

use std::io::Result as IOResult;

use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;

use std::task::Poll;

use cgroups_rs::Resources;

use futures::ready;

use nix::errno::Errno;

use nix::libc::STDERR_FILENO;
use nix::libc::STDOUT_FILENO;

use nix::fcntl::fcntl;
use nix::fcntl::FcntlArg;
use nix::fcntl::OFlag;

use nix::sys::resource::setrlimit;
use nix::sys::resource::Resource;
use nix::sys::resource::RLIM_INFINITY;

use nix::sys::signal::kill;
use nix::sys::signal::SIGCONT;
use nix::sys::signal::SIGKILL;
use nix::sys::signal::SIGSTOP;

use nix::unistd::dup2;
use nix::unistd::execv;
use nix::unistd::fork;
use nix::unistd::pipe;
use nix::unistd::read as nix_read;
use nix::unistd::ForkResult;
use nix::unistd::Pid;

use tokio::io::unix::AsyncFd;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::BufReader;
use tokio::io::Lines;

use thiserror::Error;

pub type LinuxAsyncLines = Lines<BufReader<ManagedStream>>;
type LinuxOpResult<T> = Result<T, LinuxErrorManager>;

#[derive(Debug)]
pub struct ManagedStream {
    inner: AsyncFd<OwnedFd>,
}

impl ManagedStream {
    fn new(fd: OwnedFd) -> LinuxOpResult<Self> {
        let cflags = fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL)?;
        let cflags = OFlag::from_bits_truncate(cflags);
        let nflag = cflags | OFlag::O_NONBLOCK;
        fcntl(fd.as_raw_fd(), FcntlArg::F_SETFL(nflag))?;
        let async_fd = AsyncFd::new(fd).expect("
            Asynchronous file descriptor could not be created from the process' stderr/stdout         
        ");
        Ok(Self { inner: async_fd })
    }

    pub async fn read(&self, out: &mut [u8]) -> IOResult<usize> {
        loop {
            let mut g = self.inner.readable().await?;
            match g.try_io(|inner| {
                let fd = self.inner.as_raw_fd();
                nix_read(fd, out).map_err(|e| std::io::Error::from_raw_os_error(e as i32))
            }) {
                Ok(result) => return result,
                Err(_) => continue,
            };
        }
    }
}

impl AsyncRead for ManagedStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        loop {
            let mut g = ready!(self.inner.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            match g.try_io(|inner| {
                let fd = self.inner.as_raw_fd();
                nix_read(fd, unfilled).map_err(|e| std::io::Error::from_raw_os_error(e as i32))
            }) {
                Ok(Ok(len)) => {
                    buf.advance(len);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(err)) => return Poll::Ready(Err(err)),
                Err(_) => continue,
            }
        }
    }
}

//we just have this for debug purposes right now
pub struct ManagedLinuxProcess {
    pid: Pid,
    stderr: Option<ManagedStream>,
    stdout: Option<ManagedStream>,
}

impl ManagedLinuxProcess {
    pub fn sigkill(self) -> LinuxOpResult<()> {
        kill_process_sigkill(self.pid.as_raw())?;
        Ok(())
    }

    pub fn readers(
        &mut self,
    ) -> (
        Lines<BufReader<ManagedStream>>,
        Lines<BufReader<ManagedStream>>,
    ) {
        let bout = BufReader::new(self.stdout.take().unwrap()).lines();
        let berr = BufReader::new(self.stderr.take().unwrap()).lines();
        return (bout, berr);
    }

    pub fn pid(&self) -> i32 {
        self.pid.as_raw()
    }
}

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

pub unsafe fn create_child_exec_context(
    target: String,
    args: &[String],
) -> LinuxOpResult<ManagedLinuxProcess> {
    let (stderr_read_fd, stderr_write_fd) = pipe()?;
    let (stdout_read_fd, stdout_write_fd) = pipe()?;
    match fork()? {
        ForkResult::Parent { child } => {
            let merr = ManagedStream::new(stderr_read_fd)?;
            let mout = ManagedStream::new(stdout_read_fd)?;
            let value = ManagedLinuxProcess {
                pid: child,
                stderr: Some(merr),
                stdout: Some(mout),
            };
            return Ok(value);
        }
        ForkResult::Child => {
            //we can put in the security measures here, for now, we want to set up
            //stderr and stdout
            //some processing first
            let target = CString::from_vec_unchecked(target.into_bytes());
            let args = args
                .iter()
                .map(|f| CString::from_vec_unchecked(f.clone().into_bytes()))
                .collect::<Vec<_>>();

            dup2(stderr_write_fd.as_raw_fd(), STDERR_FILENO)?;
            dup2(stdout_write_fd.as_raw_fd(), STDOUT_FILENO)?;

            execv(target.as_c_str(), &args)?;
            unreachable!("Like, obviously, this isn't supposed to happen, lol");
        }
    };
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

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_fork() {
        let v = unsafe {
            create_child_exec_context(String::from("examples/test_one.sh"), &vec![])
                .expect("creating child process failed")
        };
    }
}
