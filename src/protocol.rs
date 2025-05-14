use serde::Deserialize;
use serde::Serialize;

use termcolor::ColorSpec;
use termcolor::WriteColor;

use std::io::Write;

use crate::config::ResourceLimits;
use crate::config::ResourceProfile;
use crate::config::RestartPolicy;
use crate::config::SecurityPolicy;
use crate::config::SecurityProfile;
use crate::core::logging::LogEntry;
use crate::core::platform::linux::MemReqs;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum CommandResult {
    Success(String),
    ProcessStarted { name: String, pid: i32 },
    ProcessStopped { name: String },
    Logs { vector: Vec<LogEntry> },
    ProcessList { list: Vec<ProcessInfo> },
    ProcessDetails(ProcessDetails),
    Error(String),
}

impl CommandResult {
    pub fn display(&self) -> std::io::Result<()> {
        let mut stream = termcolor::BufferedStandardStream::stderr(termcolor::ColorChoice::Auto);

        match self {
            CommandResult::Success(msg) => {
                let mut spec = ColorSpec::new();
                spec.set_fg(Some(termcolor::Color::Green));
                stream.set_color(&spec)?;
                writeln!(stream, "Success: {}", msg)?;
            }
            CommandResult::Error(msg) => {
                let mut spec = ColorSpec::new();
                spec.set_fg(Some(termcolor::Color::Red)).set_bold(true);
                stream.set_color(&spec)?;
                writeln!(stream, "Error: {}", msg)?;
            }
            CommandResult::ProcessStarted { name, pid } => {
                let mut spec = ColorSpec::new();
                spec.set_fg(Some(termcolor::Color::Green)).set_bold(true);
                stream.set_color(&spec)?;
                writeln!(stream, "Process Started:")?;

                stream.reset()?;
                writeln!(stream, "  Name: {}", name)?;
                writeln!(stream, "  PID:  {}", pid)?;
            }
            CommandResult::ProcessStopped { name } => {
                let mut spec = ColorSpec::new();
                spec.set_fg(Some(termcolor::Color::Yellow)).set_bold(true);
                stream.set_color(&spec)?;
                writeln!(stream, "Process Stopped: {}", name)?;
            }
            CommandResult::Logs { vector } => {
                let mut spec = ColorSpec::new();
                spec.set_fg(Some(termcolor::Color::Blue)).set_bold(true);
                stream.set_color(&spec)?;
                writeln!(stream, "Log Entries:")?;
                stream.flush()?;

                for entry in vector {
                    // Use the LogEntry's own color formatting
                    let code = entry.ty.colour();
                    stream.set_color(&code)?;
                    stream.write(entry.to_fstring().as_bytes())?;
                    stream.set_color(&termcolor::ColorSpec::new())?;
                    stream.flush()?;
                }
            }
            CommandResult::ProcessList { list } => {
                let mut spec = ColorSpec::new();
                spec.set_fg(Some(termcolor::Color::Blue)).set_bold(true);
                stream.set_color(&spec)?;
                writeln!(stream, "Process List:")?;
                stream.reset()?;

                if list.is_empty() {
                    writeln!(stream, "No processes currently running")?;
                } else {
                    // Create a formatted table header
                    let mut header_spec = ColorSpec::new();
                    header_spec
                        .set_fg(Some(termcolor::Color::White))
                        .set_bold(true);
                    stream.set_color(&header_spec)?;
                    writeln!(
                        stream,
                        "{:<20} {:<7} {:<10} {:<10} {:<12}",
                        "NAME", "PID", "RUNTIME", "CPU %", "MEMORY"
                    )?;
                    stream.reset()?;

                    // Print each process
                    for proc in list {
                        // Format memory to be more human-readable (KB, MB, GB)
                        let memory = if proc.memory_usage < 1024 {
                            format!("{}KB", proc.memory_usage)
                        } else if proc.memory_usage < 1024 * 1024 {
                            format!("{:.1}MB", proc.memory_usage as f64 / 1024.0)
                        } else {
                            format!("{:.2}GB", proc.memory_usage as f64 / (1024.0 * 1024.0))
                        };

                        writeln!(
                            stream,
                            "{:<20} {:<7} {:<10} {:<10.2} {:<12}",
                            proc.name, proc.pid, proc.runtime, proc.cpu_usage, memory
                        )?;
                    }
                }
            }
            CommandResult::ProcessDetails(details) => {
                display_process_details(&mut stream, details)?;
            }
        }

        stream.reset()?;
        stream.flush()?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProcessInfo {
    pub name: String,
    pub pid: i32,
    pub runtime: String,
    pub cpu_usage: f32,
    pub memory_usage: usize,
}

//TODO: add more fields
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProcessDetails {
    pub name: String,
    pub pid: i32,
    pub vmrss_kb: usize,
    pub vmswap_kb: usize,
    pub rssanon_kb: usize,
    pub rssfile_kb: usize,
    pub rssshmem_kb: usize,
    pub uptime: usize,
    pub kernel_time: usize,
    pub nthreads: usize,
    pub read_bytes: usize,
    pub write_bytes: usize,
    pub args: Vec<String>,
    pub cwd: Option<std::path::PathBuf>,
    pub env: std::collections::HashMap<String, String>,
    pub restart: RestartPolicy,
    pub resources: ResourceLimits,
    pub security: SecurityPolicy,
}

fn display_process_details(
    stream: &mut termcolor::BufferedStandardStream,
    details: &ProcessDetails,
) -> std::io::Result<()> {
    // Header with process name
    let mut header_spec = ColorSpec::new();
    header_spec
        .set_fg(Some(termcolor::Color::Cyan))
        .set_bold(true);
    stream.set_color(&header_spec)?;
    writeln!(stream, "Process Details: {}", details.name)?;
    stream.reset()?;

    // Basic process information
    let mut section_spec = ColorSpec::new();
    section_spec
        .set_fg(Some(termcolor::Color::Blue))
        .set_bold(true);

    stream.set_color(&section_spec)?;
    writeln!(stream, "\nGeneral Information:")?;
    stream.reset()?;
    writeln!(stream, "  PID:            {}", details.pid)?;
    writeln!(stream, "  Uptime:         {} sec", details.uptime)?;
    writeln!(stream, "  Kernel Time:    {} sec", details.kernel_time)?;
    writeln!(stream, "  Thread Count:   {}", details.nthreads)?;

    // Memory usage section
    stream.set_color(&section_spec)?;
    writeln!(stream, "\nMemory Usage:")?;
    stream.reset()?;

    // Format memory values for better readability
    let format_kb = |kb: usize| -> String {
        if kb < 1024 {
            format!("{} KB", kb)
        } else if kb < 1024 * 1024 {
            format!("{:.2} MB", kb as f64 / 1024.0)
        } else {
            format!("{:.2} GB", kb as f64 / (1024.0 * 1024.0))
        }
    };

    writeln!(stream, "  RSS:            {}", format_kb(details.vmrss_kb))?;
    writeln!(stream, "  Swap:           {}", format_kb(details.vmswap_kb))?;
    writeln!(
        stream,
        "  Anonymous:      {}",
        format_kb(details.rssanon_kb)
    )?;
    writeln!(
        stream,
        "  File-backed:    {}",
        format_kb(details.rssfile_kb)
    )?;
    writeln!(
        stream,
        "  Shared Memory:  {}",
        format_kb(details.rssshmem_kb)
    )?;

    // I/O usage section
    stream.set_color(&section_spec)?;
    writeln!(stream, "\nI/O Usage:")?;
    stream.reset()?;

    // Format bytes for better readability
    let format_bytes = |bytes: usize| -> String {
        if bytes < 1024 {
            format!("{} B", bytes)
        } else if bytes < 1024 * 1024 {
            format!("{:.2} KB", bytes as f64 / 1024.0)
        } else if bytes < 1024 * 1024 * 1024 {
            format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        }
    };

    writeln!(
        stream,
        "  Read:           {}",
        format_bytes(details.read_bytes)
    )?;
    writeln!(
        stream,
        "  Written:        {}",
        format_bytes(details.write_bytes)
    )?;

    // Process execution details
    stream.set_color(&section_spec)?;
    writeln!(stream, "\nExecution Environment:")?;
    stream.reset()?;

    // Format command line arguments
    write!(
        stream,
        "  Command:        {}",
        details.args.get(0).unwrap_or(&String::from(""))
    )?;
    for arg in details.args.iter().skip(1) {
        write!(stream, " {}", arg)?;
    }
    writeln!(stream)?;

    // Working directory
    if let Some(cwd) = &details.cwd {
        writeln!(stream, "  Working Dir:    {}", cwd.display())?;
    } else {
        writeln!(stream, "  Working Dir:    Unknown")?;
    }

    // Environment variables (only show a few key ones)
    let env_keys = ["PATH", "PWD", "USER", "HOME", "SHELL", "LANG"];
    let mut env_shown = false;

    for key in env_keys.iter() {
        if let Some(value) = details.env.get(*key) {
            if !env_shown {
                writeln!(stream, "  Environment Variables:")?;
                env_shown = true;
            }
            writeln!(stream, "    {}={}", key, value)?;
        }
    }

    // Show count of additional environment variables
    let additional_vars = details.env.len()
        - env_keys
            .iter()
            .filter(|k| details.env.contains_key(**k))
            .count();
    if additional_vars > 0 {
        writeln!(stream, "    ... and {} more variables", additional_vars)?;
    }

    // Restart policy
    stream.set_color(&section_spec)?;
    writeln!(stream, "\nRestart Policy:")?;
    stream.reset()?;

    match &details.restart {
        RestartPolicy::None => writeln!(stream, "  No automatic restart")?,
        RestartPolicy::Capped(limit) => writeln!(stream, "  Restart up to {} times", limit)?,
        RestartPolicy::Infinite => writeln!(stream, "  Always restart on failure")?,
    }

    // Resource limits
    stream.set_color(&section_spec)?;
    writeln!(stream, "\nResource Limits:")?;
    stream.reset()?;

    // Resource profile
    let profile_color = match details.resources.profile {
        ResourceProfile::Light => Some(termcolor::Color::Green),
        ResourceProfile::Worker => Some(termcolor::Color::Blue),
        ResourceProfile::Service => Some(termcolor::Color::Yellow),
        ResourceProfile::Custom => Some(termcolor::Color::Magenta),
    };

    let mut profile_spec = ColorSpec::new();
    profile_spec.set_fg(profile_color);
    stream.set_color(&profile_spec)?;
    writeln!(stream, "  Profile:        {:?}", details.resources.profile)?;
    stream.reset()?;

    // CPU priority
    writeln!(
        stream,
        "  CPU Priority:   {:?}",
        details.resources.cpu_priority
    )?;

    // Memory limit
    match &details.resources.mem_limit {
        MemReqs::Unlimited => writeln!(stream, "  Memory Limit:   Unlimited")?,
        MemReqs::Specific(limit) => {
            writeln!(stream, "  Memory Limit:   {} MB", limit)?;
        }
    }

    // File descriptor limit
    if let Some(fd_limit) = details.resources.fd_limit {
        writeln!(stream, "  FD Limit:       {}", fd_limit)?;
    } else {
        writeln!(stream, "  FD Limit:       Default")?;
    }

    // Security policy
    stream.set_color(&section_spec)?;
    writeln!(stream, "\nSecurity Policy:")?;
    stream.reset()?;

    // Security profile
    let security_color = match details.security.profile {
        SecurityProfile::Default => Some(termcolor::Color::Green),
        SecurityProfile::Isolated => Some(termcolor::Color::Yellow),
        SecurityProfile::ReadOnly => Some(termcolor::Color::Blue),
        SecurityProfile::Sandboxed => Some(termcolor::Color::Red),
        SecurityProfile::Custom => Some(termcolor::Color::Magenta),
    };

    let mut security_spec = ColorSpec::new();
    security_spec.set_fg(security_color);
    stream.set_color(&security_spec)?;
    writeln!(stream, "  Profile:        {:?}", details.security.profile)?;
    stream.reset()?;

    // Security restrictions as a neat table
    writeln!(stream, "  Restrictions:")?;
    let restrictions = [
        ("Network Access", details.security.no_net),
        ("Filesystem Access", details.security.no_fs),
        ("Read-only Filesystem", details.security.read_only_fs),
        ("Process Creation", details.security.no_process),
        ("Thread Creation", details.security.no_thread),
        ("IPC", details.security.no_ipc),
        ("System Management", details.security.no_sysman),
    ];

    for (name, restricted) in restrictions.iter() {
        let (status, color) = if *restricted {
            ("Restricted", Some(termcolor::Color::Red))
        } else {
            ("Allowed", Some(termcolor::Color::Green))
        };

        write!(stream, "    {:<20}", name)?;

        let mut status_spec = ColorSpec::new();
        status_spec.set_fg(color);
        stream.set_color(&status_spec)?;
        writeln!(stream, "{}", status)?;
        stream.reset()?;
    }

    Ok(())
}
