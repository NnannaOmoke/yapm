use crate::core::platform::consts::LinuxSyscallConsts;
use crate::core::platform::traits::DisobeyPolicy;

use std::ffi::CString;

use std::io::BufRead;
use std::io::Read;
use std::io::Result as IOResult;

use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;

use std::task::Poll;

use cgroups_rs::Resources;

use futures::ready;

use libseccomp::error::SeccompError;
use libseccomp::scmp_cmp;
pub use libseccomp::ScmpAction;
use libseccomp::ScmpArgCompare;
use libseccomp::ScmpCompareOp;
use libseccomp::ScmpFilterContext;
use libseccomp::ScmpSyscall;

pub use nix::errno::Errno;

use nix::libc::syscall;
use nix::libc::SYS_pidfd_open;
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

use nix::sys::wait::waitpid;
use nix::sys::wait::WaitPidFlag;
pub use nix::sys::wait::WaitStatus;

use nix::unistd::dup2;
use nix::unistd::execv;
use nix::unistd::fork;
use nix::unistd::pipe;
use nix::unistd::read as nix_read;
use nix::unistd::ForkResult;
pub use nix::unistd::Pid;

use serde::Deserialize;
use serde::Serialize;

use tokio::io::unix::AsyncFd;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::BufReader;
use tokio::io::Lines;

use thiserror::Error;

pub type LinuxAsyncLines = Lines<BufReader<ManagedStream>>;
type LinuxOpResult<T> = Result<T, LinuxErrorManager>;

#[derive(Eq, PartialEq, Default, Debug, Clone, Copy, Deserialize, Serialize)]
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

impl std::str::FromStr for CPUPriority {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "max" => Ok(Self::Max),
            "very high" => Ok(Self::VeryHigh),
            "high" => Ok(Self::High),
            "normal" => Ok(Self::Normal),
            "low" => Ok(Self::Low),
            "very low" => Ok(Self::VeryLow),
            "lowest" => Ok(Self::Low),
            _ => Err(format!(
                "{} is not accepted as a parameter to use to tune cpu priority",
                s
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum MemReqs {
    #[default]
    Unlimited,
    Specific(usize),
}

#[derive(Debug, Error)]
pub enum LinuxErrorManager {
    #[error("SIGKILL could not terminate the running process; YAPM does not have the neccessary permissions to terminate the child process")]
    SigKillNoPerm,
    #[error(
        "SIGKILL could not terminate the target process; It either does not exist or has been terminated already"
    )]
    SigKillNoExist,
    #[error("A seccomp action has failed: {}", .0)]
    SeccompError(#[from] SeccompError),
    #[error("There has been an unknown error: {}", .0)]
    UnexpectedError(Errno),
    #[error("An IO Operation failed: {}", .0)]
    IOError(#[from] std::io::Error),
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
#[derive(Debug)]
pub struct ManagedLinuxProcess {
    pid: Pid,
    stderr: Option<ManagedStream>,
    stdout: Option<ManagedStream>,
}

impl ManagedLinuxProcess {
    pub fn sigkill(&mut self) -> LinuxOpResult<()> {
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

    pub fn wait(&mut self) -> LinuxOpResult<WaitStatus> {
        let s = waitpid(self.pid, Some(WaitPidFlag::WUNTRACED))?;
        Ok(s)
    }

    #[cfg(debug_assertions)]
    async fn read_to_string(&mut self) -> LinuxOpResult<String> {
        let mut berr = BufReader::new(self.stderr.take().unwrap());
        let mut string = String::new();
        berr.read_line(&mut string).await.expect("");
        Ok(string)
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

#[cfg(debug_assertions)]
pub unsafe fn test_create_child_exec_context(
    target: String,
    args: &[String],
    scmp_fn: fn(&mut ScmpFilterContext, DisobeyPolicy) -> LinuxOpResult<()>,
    policy: DisobeyPolicy,
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

            let mut ctx = init_seccomp_context()?;
            scmp_fn(&mut ctx, policy)?;
            load_seccomp_ctx(&mut ctx)?;

            execv(target.as_c_str(), &args).expect("Execve failed!");
            unreachable!("Like, obviously, this isn't supposed to happen, lol");
        }
    };
}

#[derive(Debug, Default)]
pub struct LinuxRuntimeMetrics {
    //
    vmrss_kb: usize,
    vmswap_kb: usize,
    vmsize_kb: usize,
    vmhwm_kb: usize,
    rssanon_kb: usize,
    rssfile_kb: usize,
    rssshmem_kb: usize,

    //cpu stuff
    uptime: usize,
    kernel_time: usize,
    children_uptime: usize,
    children_kerneltime: usize,
    nthreads: usize,

    //io stuff
    read_bytes: usize,
    write_bytes: usize,

    //file handles
    status_file: Option<std::fs::File>,
    stat_file: Option<std::fs::File>,
    io_file: Option<std::fs::File>,
}

impl LinuxRuntimeMetrics {
    ///Expects path given by: /proc/[pid]/status
    //we don't know whether we want this to be `self` until the design of the application is complete
    //this asref is for caching, we don't want to close the file handle when we're done!
    //this is the only function that will use it in the first place
    pub fn new(pid: Pid) -> LinuxOpResult<Self> {
        let mut metrics = Self::default();

        let status = format!("/proc/{}/status", pid);
        metrics.status_file = Some(Self::open_file(&status)?);
        let stat = format!("/proc/{}/stat", pid);
        metrics.stat_file = Some(Self::open_file(&stat)?);
        let io = format!("/proc/{}/io", pid);
        metrics.io_file = Some(Self::open_file(&io)?);
        Ok(metrics)
    }

    pub fn refresh(&mut self) -> LinuxOpResult<()> {
        self.parse_mem_status()?;
        self.parse_io_status()?;
        self.parse_cpu_status()?;
        Ok(())
    }

    fn open_file(path: &str) -> LinuxOpResult<std::fs::File> {
        Ok(std::fs::OpenOptions::new()
            .read(true)
            .write(false)
            .create(false)
            .append(false)
            .create_new(false)
            .open(path)?)
    }

    fn parse_mem_status(&mut self) -> LinuxOpResult<()> {
        //the next thing is that we want to go over each line and split:
        //TODO: create an illegal state error
        let f = self
            .status_file
            .as_ref()
            .ok_or(LinuxErrorManager::UnexpectedError(Errno::UnknownErrno))?;
        let bufreader = std::io::BufReader::new(f);
        let lines = bufreader.lines();
        for l in lines {
            let l = l?;
            let mut parts = l.split_whitespace();
            let k = parts.next().unwrap_or("").trim_end_matches(":");
            let value = match k {
                "VmRSS" => &mut self.vmrss_kb,
                "VmSwap" => &mut self.vmswap_kb,
                "VmSize" => &mut self.vmsize_kb,
                "VmHWM" => &mut self.vmhwm_kb,
                "RssAnon" => &mut self.rssanon_kb,
                "RssFile" => &mut self.rssfile_kb,
                "RssShmem" => &mut self.rssshmem_kb,
                _ => continue,
            };

            *value = match parts.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(v) => v,
                None => continue,
            };
        }
        Ok(())
    }
    //file handles, this is `/proc/pid/fd`
    fn parse_resource_status<T: std::ops::Deref<Target = std::fs::File>>(
        fpath: T,
    ) -> LinuxOpResult<usize> {
        let mut bufr = std::io::BufReader::new(fpath.deref());
        let mut target = String::new();
        bufr.read_to_string(&mut target)?;
        Ok(target.split_whitespace().count())
    }

    fn parse_io_status(&mut self) -> LinuxOpResult<()> {
        let f = self
            .io_file
            .as_ref()
            .ok_or(LinuxErrorManager::UnexpectedError(Errno::UnknownErrno))?;
        let bufr = std::io::BufReader::new(f);
        //now we need to parse this
        let lines = bufr.lines().take(2).collect::<Result<Vec<_>, _>>()?; //we have all the lines
                                                                          //the next thing to do is to do this: we need to get the 0th and the 1st line
        self.read_bytes = lines[0]
            .strip_prefix("rchar: ")
            .and_then(|f| f.parse::<usize>().ok())
            .ok_or(LinuxErrorManager::UnexpectedError(Errno::UnknownErrno))?;
        self.write_bytes = lines[1]
            .strip_prefix("wchar: ")
            .and_then(|f| f.parse::<usize>().ok())
            .ok_or(LinuxErrorManager::UnexpectedError(Errno::UnknownErrno))?;
        Ok(())
    }

    fn parse_cpu_status(&mut self) -> LinuxOpResult<()> {
        let mut cont = String::new();
        let f = self
            .stat_file
            .as_ref()
            .ok_or(LinuxErrorManager::UnexpectedError(Errno::UnknownErrno))?;
        let mut bufr = std::io::BufReader::new(f);
        bufr.read_to_string(&mut cont)?;
        let metrics = cont.split_whitespace().collect::<Vec<_>>();
        //we need 13: utime, 14: stime, cutime: 15, cstime: 16, nthreads: 19
        self.uptime = metrics[13]
            .parse::<usize>()
            .or(Err(LinuxErrorManager::UnexpectedError(Errno::UnknownErrno)))?;
        self.kernel_time = metrics[14]
            .parse::<usize>()
            .or(Err(LinuxErrorManager::UnexpectedError(Errno::UnknownErrno)))?;
        self.children_uptime = metrics[15]
            .parse::<usize>()
            .or(Err(LinuxErrorManager::UnexpectedError(Errno::UnknownErrno)))?;
        self.children_kerneltime = metrics[16]
            .parse::<usize>()
            .or(Err(LinuxErrorManager::UnexpectedError(Errno::UnknownErrno)))?;
        self.nthreads = metrics[19]
            .parse::<usize>()
            .or(Err(LinuxErrorManager::UnexpectedError(Errno::UnknownErrno)))?;
        Ok(())
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
            mibs as u64 * 1024 * 1024,
            (mibs as u64 + 2) * 1024 * 1024,
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

//TODO: implement the GOAT shill's idea here
pub async fn monitor_process_life(pid: Pid) -> LinuxOpResult<WaitStatus> {
    // open the pidfd
    let fd = unsafe {
        let fd = syscall(SYS_pidfd_open, pid, nix::libc::PIDFD_NONBLOCK);
        if fd < 0 {
            return Err(LinuxErrorManager::IOError(std::io::Error::last_os_error()));
        }
        //case because syscall returns c_long == i64
        std::os::fd::OwnedFd::from_raw_fd(fd as i32)
    };
    let async_fd = AsyncFd::new(fd)?;
    loop {
        let mut g = async_fd.readable().await?; //the koko of the operation, lmao

        let res = unsafe {
            nix::sys::wait::waitid(
                nix::sys::wait::Id::PIDFd(BorrowedFd::borrow_raw(async_fd.get_ref().as_raw_fd())),
                WaitPidFlag::WNOHANG | WaitPidFlag::WEXITED,
            )?
        };

        match &res {
            WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _) => return Ok(res),
            _ => {
                g.clear_ready();
                continue;
            }
        }
    }
}

// a single place to initialize a seccomp context
pub fn init_seccomp_context() -> LinuxOpResult<ScmpFilterContext> {
    let mut ctx = ScmpFilterContext::new(ScmpAction::Allow)?;
    ctx.add_arch(libseccomp::ScmpArch::Native)?;
    Ok(ctx)
}

pub fn seccomp_no_net(ctx: &mut ScmpFilterContext, policy: DisobeyPolicy) -> LinuxOpResult<()> {
    for syscall in LinuxSyscallConsts::NETWORK_SYSCALLS.iter() {
        let s = ScmpSyscall::from_name(syscall)?;
        ctx.add_rule(policy.into(), s)?;
    }
    Ok(())
}

pub fn seccomp_no_fs(ctx: &mut ScmpFilterContext, policy: DisobeyPolicy) -> LinuxOpResult<()> {
    for syscall in LinuxSyscallConsts::FS_WRITE_SYSCALLS
        .iter()
        .chain(LinuxSyscallConsts::FS_READ_SYSCALLS.iter())
        .chain(LinuxSyscallConsts::FS_CONDITIONAL_SYSCALLS.iter())
    {
        let s = ScmpSyscall::from_name(syscall)?;
        ctx.add_rule(policy.into(), s)?;
    }
    Ok(())
}

pub fn seccomp_no_escalate(
    ctx: &mut ScmpFilterContext,
    policy: DisobeyPolicy,
) -> LinuxOpResult<()> {
    for syscall in LinuxSyscallConsts::PRIVILEGE_ESCALATION_SYSCALLS.iter() {
        let s = ScmpSyscall::from_name(syscall)?;
        ctx.add_rule(policy.into(), s)?;
    }
    Ok(())
}

//disabled by default
pub fn seccomp_no_ipc(ctx: &mut ScmpFilterContext, policy: DisobeyPolicy) -> LinuxOpResult<()> {
    for syscall in LinuxSyscallConsts::IPC_SYSCALLS
        .iter()
        .chain(LinuxSyscallConsts::FILE_BASED_IPC_SYSCALLS.iter())
    {
        let s = ScmpSyscall::from_name(syscall)?;
        ctx.add_rule(policy.into(), s)?;
    }
    Ok(())
}

//enabled by default
pub fn seccomp_no_sys(ctx: &mut ScmpFilterContext, policy: DisobeyPolicy) -> LinuxOpResult<()> {
    for syscall in LinuxSyscallConsts::SYSTEM_MANIPULATION_SYSCALLS.iter() {
        let s = ScmpSyscall::from_name(syscall)?;
        ctx.add_rule(policy.into(), s)?;
    }
    Ok(())
}

//right now, this blocks execve, so we can't spawn the applications.
//we can modify the policy to allow for us to spawn the application however
//TODO: modify function signature to allow for capture of proc-name and args, so we can construct a filter
//that allows execve in such cases
pub fn seccomp_no_proc(ctx: &mut ScmpFilterContext, policy: DisobeyPolicy) -> LinuxOpResult<()> {
    let c = ScmpSyscall::from_name("clone")?;
    let cmp = scmp_cmp!($arg0 != 0x00010000);
    ctx.add_rule_conditional(ScmpAction::Log, c, &[cmp])?; //ScmpAction::Log; hack to allow forbidden syscalls be made without killing the process
                                                           //as seccomp will not allow you to use the default scmpaction for these, it's much easier to do this
    for syscall in LinuxSyscallConsts::PROCESS_CREATION_SYSCALLS
        .iter()
        .chain(LinuxSyscallConsts::PROCESS_CONTROL_SYSCALLS.iter())
    {
        let s = ScmpSyscall::from_name(syscall)?;
        ctx.add_rule(policy.into(), s)?;
    }

    Ok(())
}

pub fn seccomp_no_thread(ctx: &mut ScmpFilterContext, policy: DisobeyPolicy) -> LinuxOpResult<()> {
    let c = ScmpSyscall::from_name("clone")?;
    let cmp = ScmpArgCompare::new(0, ScmpCompareOp::Equal, 0x00010000);
    ctx.add_rule_conditional(policy.into(), c, &[cmp])?;
    Ok(())
}

pub fn seccomp_readonly_fs(
    ctx: &mut ScmpFilterContext,
    policy: DisobeyPolicy,
) -> LinuxOpResult<()> {
    for syscall in LinuxSyscallConsts::FS_WRITE_SYSCALLS.iter() {
        let s = ScmpSyscall::from_name(syscall)?;
        ctx.add_rule(policy.into(), s)?;
    }

    let open = ScmpSyscall::from_name("open")?;

    // Create a rule that blocks open calls with write access
    // Arg1 is the flags argument to open
    // Block if (flags & O_ACCMODE) == O_WRONLY or (flags & O_ACCMODE) == O_RDWR
    let wronly_cmp = ScmpArgCompare::new(
        1,
        ScmpCompareOp::MaskedEqual(nix::libc::O_ACCMODE as u64),
        nix::libc::O_WRONLY as u64,
    );
    let rdwr_cmp = ScmpArgCompare::new(
        1,
        ScmpCompareOp::MaskedEqual(nix::libc::O_ACCMODE as u64),
        nix::libc::O_RDWR as u64,
    );

    ctx.add_rule_conditional(policy.into(), open, &[wronly_cmp])?;
    ctx.add_rule_conditional(policy.into(), open, &[rdwr_cmp])?;

    let openat = ScmpSyscall::from_name("openat")?;
    let openat_wronly_cmp = ScmpArgCompare::new(
        2,
        ScmpCompareOp::MaskedEqual(nix::libc::O_ACCMODE as u64),
        nix::libc::O_WRONLY as u64,
    );
    let openat_rdwr_cmp = ScmpArgCompare::new(
        2,
        ScmpCompareOp::MaskedEqual(nix::libc::O_ACCMODE as u64),
        nix::libc::O_RDWR as u64,
    );

    ctx.add_rule_conditional(policy.into(), openat, &[openat_wronly_cmp])?;
    ctx.add_rule_conditional(policy.into(), openat, &[openat_rdwr_cmp])?;
    Ok(())
}

pub fn load_seccomp_ctx(ctx: &mut ScmpFilterContext) -> LinuxOpResult<()> {
    ctx.load()?;
    Ok(())
}

//TODO: test that read-only fs works
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

    #[tokio::test]
    async fn test_no_net() {
        let mut c = unsafe {
            test_create_child_exec_context(
                String::from("examples/test_net.sh"),
                &vec![],
                seccomp_no_net,
                DisobeyPolicy::NoPerm,
            )
            .expect("creating child process failed")
        };
        let (_, mut errst) = c.readers();
        while let Some(line) = errst.next_line().await.expect("") {
            dbg!(line);
        }
    }

    #[tokio::test]
    async fn test_no_proc() {
        let mut c = unsafe {
            match test_create_child_exec_context(
                String::from("examples/test_proc.sh"),
                &vec![],
                seccomp_no_proc,
                DisobeyPolicy::NoPerm,
            ) {
                Ok(c) => c,
                Err(e) => panic!("{:?}: {}", e, e.to_string()),
            }
        };
        let (_, mut errst) = c.readers();
        while let Some(l) = errst.next_line().await.expect("") {
            dbg!(l);
        }
    }

    #[tokio::test]
    async fn test_no_ipc() {
        let mut c = unsafe {
            match test_create_child_exec_context(
                String::from("examples/test_ipc.sh"),
                &vec![],
                seccomp_no_ipc,
                DisobeyPolicy::NoPerm,
            ) {
                Ok(c) => c,
                Err(e) => panic!("{:?}: {}", e, e.to_string()),
            }
        };
        let (_, mut errst) = c.readers();
        while let Some(l) = errst.next_line().await.expect("") {
            dbg!(l);
        }
    }

    #[tokio::test]
    async fn get_process_memory_details() {
        let c = unsafe {
            create_child_exec_context(String::from("examples/test_proc.sh"), &vec![])
                .expect("We could not spawn the process")
        };
        let pid = c.pid;
        let mut met = LinuxRuntimeMetrics::new(pid).expect("Something broke");
        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        dbg!(&met);
        dbg!(pid);
        tokio::time::sleep(tokio::time::Duration::from_secs(6)).await;
        met.refresh().expect("Well, that didn't work");
        dbg!(&met);
    }

    #[tokio::test]
    async fn test_process_lifetime_monitor() {
        let c = unsafe {
            create_child_exec_context(String::from("examples/test_proc.sh"), &vec![])
                .expect("couldn't spawn the process")
        };
        let pid = c.pid;
        dbg!(pid);
        tokio::spawn(async move {
            let status = monitor_process_life(pid)
                .await
                .expect("Something failed here");
            dbg!(status);
        });
        tokio::time::sleep(tokio::time::Duration::from_secs(9)).await;
    }
}
