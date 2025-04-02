pub struct LinuxSyscallConsts;
impl LinuxSyscallConsts {
    pub const NETWORK_SYSCALLS: [&str; 20] = [
        "socket",
        "connect",
        "accept",
        "accept4",
        "send",
        "sendto",
        "sendmsg",
        "sendmmsg",
        "recv",
        "recvfrom",
        "recvmsg",
        "recvmmsg",
        "bind",
        "listen",
        "setsockopt",
        "getsockopt",
        "getpeername",
        "getsockname",
        "sethostname",
        "setdomainname",
    ];

    pub const FS_WRITE_SYSCALLS: [&str; 29] = [
        "creat",
        "mknod",
        "mknodat",
        "mkdir",
        "mkdirat",
        "unlink",
        "unlinkat",
        "rename",
        "renameat",
        "renameat2",
        "link",
        "linkat",
        "symlink",
        "symlinkat",
        "truncate",
        "ftruncate",
        "chmod",
        "fchmod",
        "fchmodat",
        "chown",
        "fchown",
        "fchownat",
        "lchown",
        "mount",
        "umount",
        "umount2",
        "pivot_root",
        "write",
        "pwrite64",
    ];

    pub const FS_READ_SYSCALLS: [&str; 8] = [
        "read",
        "pread64",
        "readv",
        "preadv",
        "readlink",
        "readlinkat",
        "stat",
        "fstat",
    ];

    pub const FS_CONDITIONAL_SYSCALLS: [&str; 2] = ["open", "openat"];
    pub const PROCESS_CREATION_SYSCALLS: [&str; 11] = [
        "fork",
        "vfork",
        "clone",
        "clone3",
        "execve",
        "execveat",
        "prctl",       // Can be used for various process control operations
        "unshare",     // Used to dissociate parts of the process execution context
        "setns",       // Used to join namespaces
        "userfaultfd", // Can be used for advanced process manipulation
        "memfd_create",
    ];

    pub const PROCESS_CONTROL_SYSCALLS: [&str; 12] = [
        "ptrace",             // Process tracing/debugging
        "process_vm_readv",   // Read from another process's memory
        "process_vm_writev",  // Write to another process's memory
        "init_module",        // Load kernel module
        "finit_module",       // Load kernel module (file descriptor variant)
        "delete_module",      // Unload kernel module
        "kexec_load",         // Load a new kernel
        "kexec_file_load",    // Load a new kernel from file
        "sched_setscheduler", // Can be used to manipulate process behavior
        "sched_setparam",     // Can be used to manipulate process behavior
        "sched_setaffinity",  // Can be used to manipulate process execution
        "ioprio_set",
    ];
}
