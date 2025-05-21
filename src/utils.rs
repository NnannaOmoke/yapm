use std::io;
use std::io::Read;

use std::ops::Deref;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use clap::Parser;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::channel;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;

//This is where we're going to keep a lot of logic
use crate::config::*;
use crate::core::logging::*;
use crate::protocol::*;
use crate::{core::device::ProcessManager, DAEMON_SOCKET_ADDR};

pub fn entry() {
    let config = CLI::parse();
    //check if the daemon is alive, by seeing if we can connect to `DAEMON_SOCKET_ADDR`
    let daemonizable = should_daemonize(&config);
    let mut conn = match std::os::unix::net::UnixStream::connect(DAEMON_SOCKET_ADDR) {
        Ok(conn) => conn,
        Err(e)
            if e.kind() == std::io::ErrorKind::ConnectionRefused
                || e.kind() == std::io::ErrorKind::AddrNotAvailable
                || e.kind() == std::io::ErrorKind::NotFound =>
        {
            //the daemon does not exist, nobody's home
            if daemonizable {
                split();
            } else {
                let res =
                    CommandResult::Error(format!("We could not connect to the yapm daemon: {e}"));
                let _ = res.display();
                std::process::exit(1);
            }
            //split has returned, wait for a bit, then reconnect;
            std::thread::sleep(std::time::Duration::from_millis(50));
            let conn = match std::os::unix::net::UnixStream::connect(DAEMON_SOCKET_ADDR) {
                Ok(conn) => conn,
                Err(e) => {
                    let res = CommandResult::Error(format!("We could not connect to the yapm daemon: {e}. Please note that at this stage, it is most likely running and can be connected at: {DAEMON_SOCKET_ADDR}"));
                    let _ = res.display();
                    std::process::exit(1);
                }
            };
            conn
        }
        Err(e) => {
            let res = CommandResult::Error(format!("We could not connect to the yapm daemon: {e}"));
            let _ = res.display();
            std::process::exit(1);
        }
    };

    //we send our request over to the daemon
    let reply = send_task(&mut conn, &config);
    let _ = reply.display();
    if matches!(reply, CommandResult::Error(_)) {
        std::process::exit(1);
    }
    std::process::exit(0);
}

pub fn daemon_init() -> Result<(ProcessManager, tokio::net::UnixListener), CommandResult> {
    let _ = std::fs::remove_file(DAEMON_SOCKET_ADDR);
    let manager = match ProcessManager::init() {
        Ok(man) => man,
        Err(e) => return Err(e),
    };
    let _entry = manager.runtime.rt.enter();
    let listener = match tokio::net::UnixListener::bind(DAEMON_SOCKET_ADDR) {
        Ok(sock) => sock,
        Err(e) => {
            return Err(CommandResult::Error(format!(
                "Could not connect to the normal socket address"
            )));
        }
    };
    return Ok((manager, listener));
}

pub async fn daemon_listener(
    listener: tokio::net::UnixListener,
    pmanager: Arc<ProcessManager>,
    shutdown_sender: Sender<()>,
    mut shutdown: Receiver<()>,
) {
    loop {
        tokio::select! {
            biased;
            _ = shutdown.recv() => {
                break;
            }
            res = listener.accept() =>{
                let shutdown_sender = shutdown_sender.clone();
                let pmanager = pmanager.clone();
                match res {
                    Ok((stream, _)) => {
                        tokio::spawn(handle_incoming(stream, pmanager, shutdown_sender));
                    },
                    Err(e) => {
                        handle_connect_errcase(e, pmanager.clone().deref(), shutdown_sender.clone()).await;
                    }
                }
            },
        }
    }
    std::process::exit(0);
}

pub async fn handle_incoming(
    mut stream: tokio::net::UnixStream,
    manager: Arc<ProcessManager>,
    sender: Sender<()>,
) {
    let mut buffer = Vec::with_capacity(1024);
    if let Err(e) = stream.read_to_end(&mut buffer).await {
        let res = format!("Failed to read command: {e}");
        let e = CommandResult::Error(res.clone());
        manager.log(LogEntry::new(LogType::YapmErr, res));
        reply(&mut stream, &e, &manager).await;
        return;
    }
    match serde_json::from_slice(&buffer) {
        Ok(cmd) => {
            let result = handle_command(cmd, manager.deref(), sender).await;
            reply(&mut stream, &result, manager.deref()).await;
        }
        Err(e) => {
            let res = format!("The result could not be serialized from the stream: {e}");
            manager.log(LogEntry::new(LogType::YapmWarning, res.clone()));
            reply(&mut stream, &CommandResult::Error(res), manager.deref()).await;
        }
    }
}

pub fn send_task(stream: &mut UnixStream, cfg: &CLI) -> CommandResult {
    use std::io::Write;
    let serialized = match serde_json::to_string(cfg) {
        Ok(c) => c,
        Err(e) => {
            return CommandResult::Error(format!("Serialization of the CLI object failed: {e}"))
        }
    };
    if let Err(e) = stream.write_all(serialized.as_bytes()) {
        return CommandResult::Error(format!("Writing to the socket failed: {e}"));
    }
    stream.flush().unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut response = String::new();
    if let Err(e) = stream.read_to_string(&mut response) {
        return CommandResult::Error(format!("Reading the response object failed: {e}"));
    }
    match serde_json::from_str(&response) {
        Ok(s) => s,
        Err(e) => CommandResult::Error(format!(
            "Deserializing the response object failed: {e} - {response}"
        )),
    }
}

pub async fn reply(
    stream: &mut tokio::net::UnixStream,
    response: &CommandResult,
    rt: &ProcessManager,
) {
    //log failure when it happens, if it happens
    let s = match serde_json::to_string(response) {
        Ok(s) => s,
        Err(e) => {
            rt.log(LogEntry::new(
                LogType::YapmErr,
                format!("Serializing the response body failed: {e}"),
            ));
            return;
        }
    };
    match stream.write_all(s.as_bytes()).await {
        Ok(_) => {}
        Err(e) => {
            rt.log(LogEntry::new(
                LogType::YapmErr,
                format!("Writing to the stream failed: {e}"),
            ));
            return;
        }
    }

    let _ = stream.shutdown();
}

pub async fn handle_command(
    cmd: CLI,
    manager: &ProcessManager,
    shutdown: Sender<()>,
) -> CommandResult {
    match cmd.command {
        Command::Process(cmd) => handle_process_cmd(cmd, manager).await,
        Command::Config(cmd) => handle_config_cmd(cmd, manager).await,
        Command::Monitor(cmd) => handle_monitor_cmd(cmd, manager).await,
        Command::System(cmd) => handle_system_cmd(cmd, manager, shutdown).await,
    }
}

pub async fn handle_process_cmd(cmd: ProcessCommands, manager: &ProcessManager) -> CommandResult {
    match cmd {
        ProcessCommands::Start(args) => {
            let pcfg = ProcessConfig::from(args);
            return manager.start(pcfg).await;
        }
        ProcessCommands::Stop { process } => {
            return manager.stop(process).await;
        }
    }
}

pub async fn handle_config_cmd(cmd: ConfigCommands, manager: &ProcessManager) -> CommandResult {
    match cmd {
        ConfigCommands::StartConfig { config } => {
            //check that it exists
            match std::fs::exists(&config){
                Ok(true) => {},
                Ok(false) => return CommandResult::Error(format!("The path: {} does not exist", config.display())),
                Err(e) => return CommandResult::Error(format!("The existence of {} could not be confirmed; this may occur in cases where YAPM does not have permission to read the file.", config.display())) 
            }
            //read the string and parse the toml file
            let contents = if let Ok(contents) = std::fs::read_to_string(&config) {
                let toml = match toml::from_str::<ProcessConfig>(&contents) {
                    Ok(contents) => contents,
                    Err(e) => {
                        return CommandResult::Error(format!(
                            "Parsing the toml file failed: {}",
                            e.to_string()
                        ))
                    }
                };
                toml
            } else {
                return CommandResult::Error(format!(
                    "Failed to read config file: {}",
                    config.display()
                ));
            };
            return manager.start(contents).await;
        }
    }
}

pub async fn handle_monitor_cmd(cmd: MonitorCommands, manager: &ProcessManager) -> CommandResult {
    match cmd {
        MonitorCommands::List => manager.list_processes().await,
        MonitorCommands::Show { process } => manager.get_process_details(process).await,
        MonitorCommands::Log { lines } => CommandResult::Logs {
            vector: manager.get_logs(lines),
        },
        MonitorCommands::Metrics { process } => manager.get_process_details(process).await,
    }
}

pub async fn handle_system_cmd(
    cmd: SystemCommands,
    manager: &ProcessManager,
    shutdown: Sender<()>,
) -> CommandResult {
    match cmd {
        SystemCommands::Init { force } => {
            //this should be handled by the client, but if force, we want to shut down the daemon and restart it
            if force {
                manager.stop_all().await;
                let _ = shutdown.send(()).await;
                CommandResult::Success(format!("the runtimes have been terminated"))
            } else {
                CommandResult::Success(format!("The YAPM daemon has started successfully!"))
            }
        }
        SystemCommands::Status => {
            //maybe we can show YAPM stats here, i.e. how much RAM, etc
            CommandResult::Success(format!("yapmd is running"))
        }
        //we might have to handle these in another manner later
        SystemCommands::Shutdown => {
            if std::path::Path::new(DAEMON_SOCKET_ADDR).exists() {
                let _ = std::fs::remove_file(DAEMON_SOCKET_ADDR);
            }
            manager.stop_all().await;
            let _ = shutdown.send(()).await;
            CommandResult::Success(format!("Shutdown process initiated, Daemon is terminating"))
        }
    }
}

pub async fn handle_connect_errcase(e: io::Error, pmanager: &ProcessManager, shutdown: Sender<()>) {
    match e.kind() {
        std::io::ErrorKind::Interrupted => pmanager.log(LogEntry::new(
            LogType::YapmLog,
            "Listener accept interrupted, retrying.".to_string(),
        )),
        std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset => pmanager
            .log(LogEntry::new(
                LogType::YapmLog,
                format!("Client connection issue during accept: {e}, continuing."),
            )),
        std::io::ErrorKind::Other
            if e.raw_os_error() == Some(nix::libc::EMFILE)
                || e.raw_os_error() == Some(nix::libc::ENFILE) =>
        {
            pmanager
                .log(LogEntry::new(
                    LogType::YapmLog,
                    format!("Too many open file descriptors: {e}. This is critical. Attempting to shut down gracefully.")));
            let _ = shutdown.send(()).await;
        }
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AddrNotAvailable => {
            pmanager.log(LogEntry::new(
                LogType::YapmLog,
                format!("Listener socket encountered a fatal address error: {e}. Shutting down."),
            ));
            let _ = shutdown.send(()).await;
        }
        _ => {
            pmanager.log(LogEntry::new(
                LogType::YapmLog,
                format!("Unhandled listener accept error: {e}. Shutting down to be safe."),
            ));
            let _ = shutdown.send(()).await;
        }
    }
}

fn should_daemonize(cmd: &CLI) -> bool {
    matches!(
        cmd.command,
        Command::Process(ProcessCommands::Start(_)) | Command::System(SystemCommands::Init { .. })
    )
}

#[cfg(target_family = "unix")]
fn split() {
    use nix::unistd::fork;
    use nix::unistd::ForkResult;

    let pid = unsafe { fork() };
    match pid {
        Ok(ForkResult::Parent { child }) => return, //return from the function, the parent has nothing to do
        Ok(ForkResult::Child) => {
            //the first thing we do is to daemonize
            if let Err(e) = crate::core::platform::linux::daemonize() {
                let res = CommandResult::Error(format!("Could not daemonize yapmd: {e}"));
                let _ = res.display();
                std::process::exit(1);
            }
            //TODO: how do we even handle this error?
            let (manager, listener) = daemon_init().unwrap();
            let (tx, rx) = channel(1);
            //we have the manager and the listener...
            let manager = std::sync::Arc::new(manager);
            let manager_clone = manager.clone();
            manager
                .runtime
                .rt
                .block_on(async move { daemon_listener(listener, manager_clone, tx, rx).await })
        }
        Err(e) => {}
    }
}
