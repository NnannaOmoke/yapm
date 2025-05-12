use clap::Args;
use clap::Parser;
use clap::Subcommand;

use serde::Deserialize;
use serde::Serialize;

use thiserror::Error;

use crate::core::platform::linux::CPUPriority;
use crate::core::platform::linux::MemReqs;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("An error occured when parsing this object: {}", .0)]
    ParsingError(String),
    #[error("An error occured when validating the passed configuration: {}", .0)]
    ValidationError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    #[serde(default)]
    pub log_dir: std::path::PathBuf,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        let log_dir = if cfg!(target_os = "windows") {
            std::path::PathBuf::from(r"C:\ProgramData\yapm\logs")
        } else if cfg!(target_os = "linux") {
            std::path::PathBuf::from("/var/log/yapm")
        } else {
            unreachable!("We have not yet implemented YAPM for this OS");
        };
        Self { log_dir }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessConfig {
    /// The name of the process, as you want it identified
    pub name: String,
    /// The command to execute
    pub command: String,
    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
    /// Environment variables, as consumed by the process
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Resource limits
    #[serde(default)]
    pub resources: ResourceLimits,
    /// Restart policy: i.e. how we want the command to be restarted if it goes down
    #[serde(default)]
    pub restart: RestartPolicy,
    /// Security policies related to sandboxing and syscall management
    #[serde(default)]
    pub security: SecurityPolicy,
    /// Where the application logs should be dumped. Please note that if this is not specified, logs will be dumped in a more
    /// ephemeral location, where persistence is NOT guaranteed.
    #[serde(default)]
    pub log_dir: Option<std::path::PathBuf>,
}

impl ProcessConfig {
    /// Validates the process configuration, focusing on path existence and validity
    pub fn validate(&self) -> Result<(), String> {
        // First, validate any security policies that might have conflicts
        self.security.validate()?;

        // Now focus on validating paths
        self.validate_paths()?;

        Ok(())
    }

    /// Validates that all paths in the configuration exist and are appropriate for their purpose
    fn validate_paths(&self) -> Result<(), String> {
        // Check working directory if specified
        if let Some(cwd) = &self.cwd {
            // Check that it exists
            if !cwd.exists() {
                return Err(format!(
                    "Working directory does not exist: {}",
                    cwd.display()
                ));
            }

            // Check that it's a directory, not a file
            if !cwd.is_dir() {
                return Err(format!(
                    "Working directory path is not a directory: {}",
                    cwd.display()
                ));
            }

            // Check if it's readable (important for execution)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = std::fs::metadata(cwd) {
                    let permissions = metadata.permissions();
                    if permissions.mode() & 0o400 == 0 {
                        return Err(format!(
                            "Working directory is not readable: {}",
                            cwd.display()
                        ));
                    }
                } else {
                    return Err(format!(
                        "Unable to check permissions on working directory: {}",
                        cwd.display()
                    ));
                }
            }
        }

        // Check log directory if specified
        if let Some(log_dir) = &self.log_dir {
            // Check if it exists
            if !log_dir.exists() {
                return Err(format!(
                    "Log directory does not exist: {}",
                    log_dir.display()
                ));
            }

            // Check if it's a directory
            if !log_dir.is_dir() {
                return Err(format!(
                    "Log directory path is not a directory: {}",
                    log_dir.display()
                ));
            }

            // Check if it's writable (critical for logging)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = std::fs::metadata(log_dir) {
                    let permissions = metadata.permissions();
                    if permissions.mode() & 0o200 == 0 {
                        return Err(format!(
                            "Log directory is not writable: {}",
                            log_dir.display()
                        ));
                    }
                } else {
                    return Err(format!(
                        "Unable to check permissions on log directory: {}",
                        log_dir.display()
                    ));
                }
            }

            #[cfg(windows)]
            {
                // Create a temporary file to test write permissions
                let test_file = log_dir.join(".write_test_temp");
                match std::fs::File::create(&test_file) {
                    Ok(_) => {
                        // Clean up test file
                        let _ = std::fs::remove_file(test_file);
                    }
                    Err(_) => {
                        return Err(format!(
                            "Log directory is not writable: {}",
                            log_dir.display()
                        ));
                    }
                }
            }
        }

        let cmd_path = std::path::Path::new(&self.command);
        if cmd_path.is_absolute() || self.command.contains('/') || self.command.contains('\\') {
            if !cmd_path.exists() {
                return Err(format!(
                    "Command executable does not exist: {}",
                    self.command
                ));
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = std::fs::metadata(cmd_path) {
                    let permissions = metadata.permissions();
                    if permissions.mode() & 0o100 == 0 {
                        return Err(format!("Command is not executable: {}", self.command));
                    }
                } else {
                    return Err(format!(
                        "Unable to check permissions on command: {}",
                        self.command
                    ));
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceLimits {
    /// Built-in resource profile to use (applies preset values unless overriden)
    #[serde(default)]
    pub profile: ResourceProfile,

    /// CPU priority: This is a suggestion given to the Operating System on how well your process should be treated
    #[serde(default)]
    pub cpu_priority: CPUPriority,

    /// Memory limit, in MB
    #[serde(default)]
    pub mem_limit: MemReqs,

    /// File descriptor limit
    pub fd_limit: Option<usize>,
}

//placeholder to allow proper deserialization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawResourceLimits {
    /// Built-in resource profile to use (applies preset values unless overriden)
    pub profile: Option<ResourceProfile>,

    /// CPU priority: This is a suggestion given to the Operating System on how well your process should be treated
    pub cpu_priority: Option<CPUPriority>,

    /// Memory limit, in MB
    pub mem_limit: Option<MemReqs>,

    /// File descriptor limit
    pub fd_limit: Option<usize>,
}

impl<'de> Deserialize<'de> for ResourceLimits {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let args: RawResourceLimits = RawResourceLimits::deserialize(deserializer)?;
        let mut limits = ResourceLimits::from_profile(args.profile.unwrap_or_default());

        if let Some(priority_str) = &args.cpu_priority {
            limits.cpu_priority = *priority_str;
        }

        if let Some(mem_str) = &args.mem_limit {
            limits.mem_limit = mem_str.clone();
        }

        if let Some(fd) = args.fd_limit {
            limits.fd_limit = Some(fd);
        }

        Ok(limits)
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            profile: ResourceProfile::Custom,
            cpu_priority: CPUPriority::default(),
            mem_limit: MemReqs::default(),
            fd_limit: None,
        }
    }
}

impl ResourceLimits {
    pub fn from_profile(profile: ResourceProfile) -> Self {
        match profile {
            ResourceProfile::Light => Self {
                profile,
                cpu_priority: CPUPriority::Low,
                mem_limit: MemReqs::Specific(100), // 100MB
                fd_limit: Some(32),
            },
            ResourceProfile::Worker => Self {
                profile,
                cpu_priority: CPUPriority::Normal,
                mem_limit: MemReqs::Specific(500), // 500MB
                fd_limit: Some(128),
            },
            ResourceProfile::Service => Self {
                profile,
                cpu_priority: CPUPriority::High,
                mem_limit: MemReqs::Specific(1024), // 1GB
                fd_limit: Some(1024),
            },
            ResourceProfile::Custom => Self::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum RestartPolicy {
    #[default]
    None,
    Capped(usize),
    Infinite,
}

impl RestartPolicy {
    fn parse(value: &str) -> Result<Self, String> {
        let t = value.parse::<usize>();
        match t {
            Ok(value) => return Ok(Self::Capped(value)),
            Err(_) => {}
        }

        let v = value.to_lowercase();
        match v.as_str(){
            "none" => Ok(Self::None),
            "infinite" => Ok(Self::Infinite),
            _ => Err(String::from("Could not parse the provided input: Options are: a non-negative number, or 'none' or 'infinite'"))        
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Restarts {
    #[serde(default)]
    policy: RestartPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawSecurityPolicy {
    /// Security profile to use
    pub profile: Option<SecurityProfile>,

    /// Whether to restrict network access on a syscall level
    pub no_net: Option<bool>,

    /// Whether to restrict filesystem read/write/modify access on a syscall level
    pub no_fs: Option<bool>,

    /// Whether to make filesystem access read-only
    pub read_only_fs: Option<bool>,

    /// Whether to restrict spawning processes
    pub no_process: Option<bool>,

    /// Whether to prevent thread creation, i.e to force the application to only use one thread
    pub no_thread: Option<bool>,

    /// Whether to prevent interprocess communication
    pub no_ipc: Option<bool>,

    /// Whether to prevent system manipulation. This is always true by default
    pub no_sysman: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecurityPolicy {
    /// Security profile to use
    #[serde(default)]
    pub profile: SecurityProfile,

    /// Whether to restrict network access on a syscall level
    #[serde(default)]
    pub no_net: bool,

    /// Whether to restrict filesystem read/write/modify access on a syscall level
    #[serde(default)]
    pub no_fs: bool,

    /// Whether to make filesystem access read-only
    #[serde(default)]
    pub read_only_fs: bool,

    /// Whether to restrict spawning processes
    #[serde(default)]
    pub no_process: bool,

    /// Whether to prevent thread creation, i.e to force the application to only use one thread
    #[serde(default)]
    pub no_thread: bool,

    /// Whether to prevent interprocess communication
    #[serde(default)]
    pub no_ipc: bool,

    /// Whether to prevent system manipulation. This is always true by default
    #[serde(default)]
    pub no_sysman: bool,
}

impl<'de> Deserialize<'de> for SecurityPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let args: RawSecurityPolicy = RawSecurityPolicy::deserialize(deserializer)?;
        let mut limits = SecurityPolicy::from_profile(args.profile.unwrap_or_default());

        if let Some(b) = args.no_net {
            limits.no_net = b;
        }
        if let Some(b) = args.read_only_fs {
            limits.read_only_fs = b;
        }
        if let Some(b) = args.no_process {
            limits.no_process = b;
        }
        if let Some(b) = args.no_thread {
            limits.no_thread = b;
        }
        if let Some(b) = args.no_ipc {
            limits.no_ipc = b;
        }
        if let Some(b) = args.no_sysman {
            limits.no_sysman = b;
        }
        Ok(limits)
    }
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            profile: SecurityProfile::default(),
            no_net: false,
            no_fs: false,
            read_only_fs: false,
            no_process: false,
            no_thread: false,
            no_ipc: false,
            no_sysman: true,
        }
    }
}

#[derive(Parser, Debug, Clone, Serialize, Deserialize)]
#[command(name = "yapm", author, version, about, long_about)]
pub struct CLI {
    #[arg(global = true, long, value_name = "LOG_DIR")]
    pub log_dir: Option<std::path::PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Debug, Subcommand, Serialize, Deserialize)]
pub enum Command {
    /// Process management controls
    #[command(subcommand)]
    Process(ProcessCommands),

    /// Configuration management controls
    #[command(subcommand)]
    Config(ConfigCommands),

    /// Monitoring and information commands
    #[command(subcommand)]
    Monitor(MonitorCommands),

    /// System level commands
    #[command(subcommand)]
    System(SystemCommands),
}

#[derive(Clone, Debug, Subcommand, Serialize, Deserialize)]
pub enum ProcessCommands {
    /// Start a new process with the specified arguments
    #[command(visible_alias = "s")]
    Start(StartArgs),

    /// Start a running process; you can specify a name, or if you have it, a process ID
    #[command(visible_alias = "kill")]
    Stop {
        #[arg(value_name = "PROCESS_INFO")]
        process: String,
    },
}

#[derive(Clone, Debug, Subcommand, Serialize, Deserialize)]
pub enum ConfigCommands {
    /// Start a process using a TOML configuration file to define the parameters
    #[command(alias = "run")]
    StartConfig {
        #[arg(value_name = "FILE")]
        config: std::path::PathBuf,
    },
    //TODO: add support for generating configuration files
}

#[derive(Clone, Debug, Subcommand, Serialize, Deserialize)]
pub enum MonitorCommands {
    /// List all running processes, and a brief summary of their resource consumption
    #[command(alias = "ls")]
    List,
    /// Get process details, i.e. YAPM permissions, and slightly more detailed resource consumption metrics
    #[command(alias = "info")]
    Show {
        #[arg(value_name = "PROCESS_INFO")]
        process: String,
    },
    /// Get general YAPM logs, i.e. of YAPM itself, and the processes it manages
    Log {
        #[arg(short, long, default_value = "20")]
        lines: usize,
    },
    /// Show detailed resource metrics for a particular process
    Metrics {
        #[arg(value_name = "PROCESS_INFO")]
        process: String,
    },
}

#[derive(Clone, Debug, Subcommand, Serialize, Deserialize)]
pub enum SystemCommands {
    /// Initialize the YAPM daemon
    Init {
        /// Force reinitializing even if the daemon is already running
        #[arg(short, long)]
        force: bool,
    },
    /// Check the status of the YAPM daemon
    Status,
    ///Shutdown the daemon, thus terminating all managed processes
    Shutdown,
}

#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct StartArgs {
    /// Process name
    #[arg(short, long, value_name = "NAME")]
    pub name: String,

    /// The command to execute, this is the target
    #[arg(short, long, value_name = "CMD")]
    pub command: String,

    /// Command specific arguments to pass to the target
    /// For executing scripts with an interpreter, this is usually the filename of whatever you want to run
    /// You can also pass specific args to your script too
    #[arg(value_name = "ARG", trailing_var_arg = true)]
    pub args: Vec<String>,

    /// Working directory to execute the process in
    #[arg(value_name = "DIR", long)]
    pub cwd: Option<std::path::PathBuf>,

    /// Environment variables to execute the target with
    #[arg(short, long, value_parser = parse_kv_pair, value_name = "KEY=VALUE")]
    pub env: Vec<(String, String)>,

    /// This is the directory in which logs for this process should be kept
    /// Relying on default logs has the danger that it might be ephemeral, so it is best to specify one
    #[arg(long)]
    pub log_dir: Option<std::path::PathBuf>,

    /// Describes how the process manager should handle restarts on failure
    /// The restart policy is as follows: 'none', 'infinite' or a number of retries
    #[arg(long, value_parser = RestartPolicy::parse)]
    pub restart: Option<RestartPolicy>,

    /// Resource management options
    #[command(flatten)]
    pub resources: ResourceArgs,

    /// Security policy options
    #[command(flatten)]
    pub security: SecurityArgs,
}

impl From<StartArgs> for ProcessConfig {
    fn from(args: StartArgs) -> Self {
        let security_policy = SecurityPolicy::from(&args.security);
        let resource_limits = ResourceLimits::from(&args.resources);

        Self {
            name: args.name,
            command: args.command,
            args: args.args,
            cwd: args.cwd,
            env: args.env.into_iter().collect(),
            resources: resource_limits,
            restart: args.restart.unwrap_or_default(),
            security: security_policy,
            log_dir: args.log_dir,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SecurityProfile {
    #[default]
    /// Default security settings (minimal restrictions)
    Default,
    /// Isolated mode - no network, process spawning, or IPC
    Isolated,
    /// Readonly: -file system is read-only
    ReadOnly,
    /// Sandboxed mode - heavily restricted with minimal privileges
    Sandboxed,
    /// Custom security settings
    Custom,
}

impl std::str::FromStr for SecurityProfile {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "default" => Ok(Self::Default),
            "isolated" => Ok(Self::Isolated),
            "readonly" => Ok(Self::ReadOnly),
            "sandboxed" => Ok(Self::Sandboxed),
            "custom" => Ok(Self::Custom),
            _ => Err(ConfigError::ParsingError(format!("Unknown security profile: {}. Valid options are: default, isolated, readonly, sandboxed, custom", s))),
        }
    }
}

impl std::fmt::Display for SecurityProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::Isolated => write!(f, "isolated"),
            Self::ReadOnly => write!(f, "readonly"),
            Self::Sandboxed => write!(f, "sandboxed"),
            Self::Custom => write!(f, "custom"),
        }
    }
}

impl SecurityPolicy {
    /// Create a security policy from a profile
    pub fn from_profile(profile: SecurityProfile) -> Self {
        match profile {
            SecurityProfile::Default => Self::default(),
            SecurityProfile::Isolated => Self {
                profile,
                no_net: true,
                no_process: true,
                no_ipc: true,
                ..Self::default()
            },
            SecurityProfile::ReadOnly => Self {
                profile,
                read_only_fs: true,
                ..Self::default()
            },
            SecurityProfile::Sandboxed => Self {
                profile,
                no_net: true,
                read_only_fs: true,
                no_process: true,
                no_thread: true,
                no_ipc: true,
                no_sysman: true,
                ..Self::default()
            },
            SecurityProfile::Custom => Self {
                profile: SecurityProfile::Custom,
                ..Self::default()
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        // Check for contradictions
        if self.no_fs && self.read_only_fs {
            return Err("Both no_fs and read_only_fs are set; no_fs takes precedence".to_string());
        }

        Ok(())
    }
}

/// Security-related command line arguments
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct SecurityArgs {
    /// Security profile to use (default, isolated, readonly, sandboxed, custom)
    #[arg(long, value_name = "PROFILE")]
    pub security_profile: Option<SecurityProfile>,

    /// Restrict network access
    #[arg(long)]
    pub no_net: bool,

    /// Make filesystem read-only
    #[arg(long)]
    pub readonly_fs: bool,

    /// Prevent process creation
    #[arg(long)]
    pub no_process: bool,

    /// Prevent thread creation
    #[arg(long)]
    pub no_thread: bool,

    /// Prevent IPC
    #[arg(long)]
    pub no_ipc: bool,

    /// Allow system manipulation (default: disallowed)
    #[arg(long)]
    pub allow_sysman: bool,
}

/// Convert security args to security policy
impl From<&SecurityArgs> for SecurityPolicy {
    fn from(args: &SecurityArgs) -> Self {
        // Start with the appropriate profile
        let mut policy = if let Some(profile) = args.security_profile {
            Self::from_profile(profile)
        } else {
            Self::default()
        };

        // Apply explicit overrides
        if args.no_net {
            policy.no_net = true;
        }

        if args.readonly_fs {
            policy.read_only_fs = true;
        }

        if args.no_process {
            policy.no_process = true;
        }

        if args.no_thread {
            policy.no_thread = true;
        }

        if args.no_ipc {
            policy.no_ipc = true;
        }

        if args.allow_sysman {
            policy.no_sysman = false;
        }

        policy
    }
}

/// Resource profile presets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ResourceProfile {
    /// Minimal resouce usage for minimal utilities
    Light,
    /// Reasonable resource usage for a background service
    #[default]
    Worker,
    /// Higher resource usage for full services
    Service,
    /// Custom resource profile
    Custom,
}

impl std::str::FromStr for ResourceProfile {
    type Err = ConfigError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "light" => Ok(Self::Light),
            "worker" => Ok(Self::Worker),
            "service" => Ok(Self::Service),
            "custom" => Ok(Self::Custom),
            _ => Err(ConfigError::ParsingError(format!(
                "Unknown resource profile: {}, Valid options are: light, worker, service, custom",
                s
            ))),
        }
    }
}

impl std::fmt::Display for ResourceProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Light => write!(f, "light"),
            Self::Worker => write!(f, "worker"),
            Self::Service => write!(f, "service"),
            Self::Custom => write!(f, "custom"),
        }
    }
}

// TODO: we might need dedicated parsers for this shit
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ResourceArgs {
    /// Built in resource to use (light, worker, service, custom)
    #[arg(long, value_name = "PROFILE")]
    pub resource_profile: Option<ResourceProfile>,

    ///CPU priority to use: (low, normal, high)
    #[arg(long, value_name = "PRIORITY")]
    pub cpu_priority: Option<String>,

    /// Memory Limit in MB (or "unlimited")
    #[arg(long, value_name = "MB")]
    pub memory_limit: Option<String>,

    /// File descriptor limit. Internally we'll add +2 to account for stderr and stdout
    #[arg(long, value_name = "N")]
    pub fd_limit: Option<usize>,
}

impl From<&ResourceArgs> for ResourceLimits {
    fn from(args: &ResourceArgs) -> Self {
        let mut limits = if let Some(profile) = args.resource_profile {
            Self::from_profile(profile)
        } else {
            Self::default()
        };

        if let Some(priority_str) = &args.cpu_priority {
            if let Ok(priority) = priority_str.parse() {
                limits.cpu_priority = priority;
            }
        }

        if let Some(mem_str) = &args.memory_limit {
            if mem_str.to_lowercase() == "unlimited" {
                limits.mem_limit = MemReqs::Unlimited;
            } else if let Ok(mem) = mem_str.parse::<usize>() {
                limits.mem_limit = MemReqs::Specific(mem);
            }
        }

        if let Some(fd) = args.fd_limit {
            limits.fd_limit = Some(fd);
        }

        limits
    }
}

// //TODO: define a better error type later
// pub fn load_config_file(cli: &CLI) -> Result<AppConfig, String> {
//     let mut cfg = AppConfig {
//         global: GlobalConfig::default(),
//     };
// }

fn parse_kv_pair(s: &str) -> Result<(String, String), String> {
    let p = s
        .find('=')
        .ok_or_else(|| "Invalid KEY=VALUE pairing structure".to_string())?;
    Ok((s[..p].to_string(), s[p + 1..].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use toml;

    #[test]
    fn test_cli_and_toml_equivalence() {
        // Create CLI arguments
        let cli_args = vec![
            "yapm",
            "process",
            "start",
            "--name",
            "test-service",
            "--command",
            "/usr/bin/python3",
            "--cwd",
            "/tmp/workdir",
            "--env",
            "KEY1=value1",
            "--env",
            "KEY2=value2",
            "--log-dir",
            "/var/log/test",
            "--restart",
            "infinite",
            "--resource-profile",
            "worker",
            "--cpu-priority",
            "high",
            "--memory-limit",
            "512",
            "--fd-limit",
            "128",
            "--security-profile",
            "isolated",
            "arg1",
            "arg2",
            "arg3",
        ];

        // Parse CLI arguments
        let parsed_cli = CLI::try_parse_from(cli_args).expect("Failed to parse CLI args");

        // Extract the start args from the CLI
        let start_args = match parsed_cli.command {
            Command::Process(ProcessCommands::Start(args)) => args,
            _ => panic!("Unexpected command type"),
        };

        // Convert to ProcessConfig
        let cli_config = ProcessConfig::from(start_args);

        // Create equivalent TOML string
        let toml_str = r#"
            name = "test-service"
            command = "/usr/bin/python3"
            args = ["arg1", "arg2", "arg3"]
            cwd = "/tmp/workdir"
            restart = "Infinite"
            log_dir = "/var/log/test"

            
            [env]
            KEY1 = "value1"
            KEY2 = "value2"
            
            [resources]
            profile = "Worker"
            cpu_priority = "High"
            mem_limit = {Specific = 512}
            fd_limit = 128
                        
            [security]
            profile = "isolated"            
        "#;

        // Parse TOML string
        let toml_config: ProcessConfig = toml::from_str(toml_str).expect("Failed to parse TOML");

        // Compare the two configs
        assert_eq!(cli_config.name, toml_config.name);
        assert_eq!(cli_config.command, toml_config.command);
        assert_eq!(cli_config.args, toml_config.args);
        assert_eq!(cli_config.cwd, toml_config.cwd);
        assert_eq!(cli_config.env, toml_config.env);
        assert_eq!(cli_config.log_dir, toml_config.log_dir);

        // Compare restart policies
        match (&cli_config.restart, &toml_config.restart) {
            (RestartPolicy::Infinite, RestartPolicy::Infinite) => {}
            _ => panic!("Restart policies don't match"),
        }

        // Compare resource limits
        assert_eq!(cli_config.resources.profile, toml_config.resources.profile);
        assert_eq!(
            cli_config.resources.cpu_priority,
            toml_config.resources.cpu_priority
        );
        assert_eq!(
            cli_config.resources.mem_limit,
            toml_config.resources.mem_limit
        );
        assert_eq!(
            cli_config.resources.fd_limit,
            toml_config.resources.fd_limit
        );

        // Compare security policies
        assert_eq!(cli_config.security.profile, toml_config.security.profile);
        dbg!(&cli_config.security);
        dbg!(&toml_config.security);
        assert_eq!(cli_config.security.no_net, toml_config.security.no_net);
        assert_eq!(cli_config.security.no_fs, toml_config.security.no_fs);
        assert_eq!(
            cli_config.security.read_only_fs,
            toml_config.security.read_only_fs
        );
        assert_eq!(
            cli_config.security.no_process,
            toml_config.security.no_process
        );
        assert_eq!(
            cli_config.security.no_thread,
            toml_config.security.no_thread
        );
        assert_eq!(cli_config.security.no_ipc, toml_config.security.no_ipc);
        assert_eq!(
            cli_config.security.no_sysman,
            toml_config.security.no_sysman
        );
    }

    #[test]
    fn test_partial_cli_and_toml_equivalence() {
        // Test with minimal CLI arguments
        let cli_args = vec![
            "yapm",
            "process",
            "start",
            "--name",
            "minimal-service",
            "--command",
            "/bin/echo",
            "hello world",
        ];

        // Parse CLI arguments
        let parsed_cli = CLI::try_parse_from(cli_args).expect("Failed to parse CLI args");

        // Extract the start args from the CLI
        let start_args = match parsed_cli.command {
            Command::Process(ProcessCommands::Start(args)) => args,
            _ => panic!("Unexpected command type"),
        };

        // Convert to ProcessConfig
        let cli_config = ProcessConfig::from(start_args);

        // Create equivalent TOML string
        let toml_str = r#"
            name = "minimal-service"
            command = "/bin/echo"
            args = ["hello world"]
        "#;

        // Parse TOML string
        let toml_config: ProcessConfig = toml::from_str(toml_str).expect("Failed to parse TOML");

        // Compare the two configs
        assert_eq!(cli_config.name, toml_config.name);
        assert_eq!(cli_config.command, toml_config.command);
        assert_eq!(cli_config.args, toml_config.args);

        // Both should have default values for unspecified fields
        assert_eq!(cli_config.cwd, toml_config.cwd);
        assert_eq!(cli_config.env, toml_config.env);
        assert_eq!(cli_config.log_dir, toml_config.log_dir);

        // Compare resource limits (should be defaults)
        assert_eq!(cli_config.resources.profile, toml_config.resources.profile);
        assert_eq!(
            cli_config.resources.cpu_priority,
            toml_config.resources.cpu_priority
        );
        assert_eq!(
            cli_config.resources.mem_limit,
            toml_config.resources.mem_limit
        );
        assert_eq!(
            cli_config.resources.fd_limit,
            toml_config.resources.fd_limit
        );

        // Compare security policies (should be defaults)
        assert_eq!(cli_config.security.profile, toml_config.security.profile);
        assert_eq!(cli_config.security.no_net, toml_config.security.no_net);
        assert_eq!(cli_config.security.no_fs, toml_config.security.no_fs);
        assert_eq!(
            cli_config.security.read_only_fs,
            toml_config.security.read_only_fs
        );
        assert_eq!(
            cli_config.security.no_process,
            toml_config.security.no_process
        );
        assert_eq!(
            cli_config.security.no_thread,
            toml_config.security.no_thread
        );
        assert_eq!(cli_config.security.no_ipc, toml_config.security.no_ipc);
        assert_eq!(
            cli_config.security.no_sysman,
            toml_config.security.no_sysman
        );
    }

    #[test]
    fn test_validation_with_invalid_paths() {
        // Create a config with non-existent paths
        let mut config = ProcessConfig {
            name: "invalid-paths".to_string(),
            command: "/usr/bin/does-not-exist".to_string(),
            args: vec!["arg1".to_string()],
            cwd: Some(PathBuf::from("/path/that/does/not/exist")),
            env: HashMap::new(),
            resources: ResourceLimits::default(),
            restart: RestartPolicy::None,
            security: SecurityPolicy::default(),
            log_dir: Some(PathBuf::from("/another/invalid/path")),
        };

        // Validation should fail due to invalid paths
        let result = config.validate();
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error.contains("does not exist"),
            "Error message should mention that path doesn't exist"
        );

        // Now test with just invalid command
        config.cwd = None;
        config.log_dir = None;

        let result = config.validate();
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error.contains("Command"),
            "Error message should mention command issue"
        );
    }

    #[test]
    fn test_security_policy_validation() {
        // Create a security policy with conflicting settings
        let policy = SecurityPolicy {
            profile: SecurityProfile::Custom,
            no_fs: true,
            read_only_fs: true, // This conflicts with no_fs
            no_process: false,
            no_thread: false,
            no_ipc: false,
            no_sysman: true,
            ..Default::default()
        };

        // Validation should fail due to conflicting settings
        let result = policy.validate();
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error.contains("no_fs") && error.contains("read_only_fs"),
            "Error message should mention both conflicting settings"
        );

        // Create a valid security policy
        let valid_policy = SecurityPolicy {
            profile: SecurityProfile::Custom,
            no_fs: false,
            read_only_fs: true, // No conflict now
            no_process: true,
            no_thread: true,
            no_ipc: true,
            no_sysman: true,
            ..Default::default()
        };

        // Validation should pass
        assert!(valid_policy.validate().is_ok());
    }
}
