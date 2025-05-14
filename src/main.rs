#![allow(dead_code)]
#![allow(unused_variables)]

use core::device::Device;
use protocol::*;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
};

use clap::{self, Parser};

use config::{ProcessCommands, ProcessConfig, SystemCommands, CLI};

use serde_json;

use threads::TGlobalAsyncIOManager;
use tokio::{
    self,
    io::{AsyncReadExt, AsyncWriteExt},
};
use toml;

mod config;
mod core;
mod protocol;
mod threads;

const DAEMON_SOCKET_ADDR: &'static str = "/tmp/yapmd.sock";

pub fn main() {
    let cfg = crate::config::CLI::parse();
    //if we're asked to init the daemon
    if let config::Command::System(SystemCommands::Init { force }) = &cfg.command {
        //check if the daemon exists in the first place
        if let Ok(mut c) = std::os::unix::net::UnixStream::connect(DAEMON_SOCKET_ADDR) {
            //the daemon is running, if it is, kill it and turn this into the daemon
            if *force {
                let response = send_task(&mut c, &cfg);
                //sleep for a bit so that the daemon can clean everything up
                std::thread::sleep(std::time::Duration::from_millis(500));
                if matches!(response, CommandResult::Success(_)) {
                    self_daemonize_and_process(cfg);
                }
            } else {
                //we dont' need to do anything
                let _ = CommandResult::Success(format!("The daemon is already running!")).display();
            }
        } else {
            //we can't connect to the address, nobody's home
            self_daemonize_and_process(cfg);
        }
    }

    let mut c = match std::os::unix::net::UnixStream::connect(DAEMON_SOCKET_ADDR) {
        Ok(c) => c,

        Err(e) => {
            let emsg =
                CommandResult::Error(format!("We could not connect to the YAPM daemon: {e}"));
            let _ = emsg.display();
            std::process::exit(1);
        }
    };
    let response = send_task(&mut c, &cfg);
    let _ = response.display();
    if matches!(response, CommandResult::Error(_)) {
        std::process::exit(1);
    }
}

pub fn self_daemonize_and_process(config: CLI) -> ! {
    let _ = std::fs::remove_file(DAEMON_SOCKET_ADDR);
    if let Err(e) = crate::core::platform::linux::daemonize() {
        let res = CommandResult::Error(format!("Could not daemonize the process: {e}"));
        let _ = res.display();
        std::process::exit(1);
    };

    let file = std::fs::File::create("yapm-debug.log").unwrap();
    let subs = tracing_subscriber::fmt()
        .with_writer(file)
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_target(true)
        .pretty()
        .finish();
    tracing::subscriber::set_global_default(subs).unwrap();

    let span = tracing::Span::current();
    let _enter = span.enter();

    let device_state = Device::new();
    //TODO: see the rules for static initialization
    let rdevice_state = std::sync::Arc::new(device_state);

    let runtime = match crate::threads::TGlobalAsyncIOManager::new(rdevice_state.clone()) {
        Ok(rt) => rt,
        Err(e) => {
            let res =
                CommandResult::Error(format!("Could not initialize the runtime manager: {e}"));
            let _ = res.display();
            std::process::exit(1);
        }
    };

    let rclone = runtime.clone();
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    runtime.rt.block_on(async move {

        let result = handle_command(
            config,
            rdevice_state.clone(),
            rclone.clone(),
            shutdown.clone()).await;
        let listener = match tokio::net::UnixListener::bind(DAEMON_SOCKET_ADDR) {
                Ok(sock) => sock,
                Err(e) => {
                    let res = CommandResult::Error(format!("Could not bind to IPC channel: {e}"));
                    let _ = res.display();
                    std::process::exit(1);
                }
        };
        loop {
            let sclone = shutdown.clone();
            match listener.accept().await {
                Ok((mut stream, _addr)) => {
                    let runtime_clone = rclone.clone();
                    let device_clone = rdevice_state.clone();
                    let shutdown_clone = sclone.clone();
                    tokio::spawn(async move {
                        let mut buffer = Vec::new();
                        if let Err(e) = stream.read_to_end(&mut buffer).await {
                            let e = CommandResult::Error(format!("Failed to read command: {e}"));
                            reply(&mut stream, &e, &runtime_clone).await;
                            return; //huh?
                        }

                        match serde_json::from_slice(&buffer) {
                            Ok(cmd) => {
                                let result = handle_command(
                                    cmd,
                                    device_clone.clone(),
                                    runtime_clone.clone(),
                                    shutdown_clone,
                                )
                                .await;
                                reply(&mut stream, &result, &runtime_clone).await
                            }
                            Err(e) => {
                                let emsg = CommandResult::Error(format!(
                                    "The result could not be serialized from the stream: {e}"
                                ));
                                reply(&mut stream, &emsg, &runtime_clone).await
                            }
                        }
                        //WORKAROUDN TO GET THIS TO WORK
                        if sclone.load(std::sync::atomic::Ordering::SeqCst) {
                            runtime_clone
                                .log(
                                    "Daemon listener loop finished. Process will now exit.".to_string(),
                                    core::logging::LogType::YapmLog,
                                )
                                .await;
                            let _ = std::fs::remove_file(DAEMON_SOCKET_ADDR);
                            std::process::exit(0);
                        }
                    });
                }
                Err(e) => {
                    match e.kind() {
                        std::io::ErrorKind::Interrupted => {
                            rclone.log("Listener accept interrupted, retrying.".to_string(), core::logging::LogType::YapmLog).await;
                        }
                        std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset => {
                            rclone.log(format!("Client connection issue during accept: {e}, continuing."), core::logging::LogType::YapmLog).await;
                        }
                        std::io::ErrorKind::Other if e.raw_os_error() == Some(nix::libc::EMFILE) || e.raw_os_error() == Some(nix::libc::ENFILE) => {
                            rclone.log(format!("Too many open file descriptors: {e}. This is critical. Attempting to shut down gracefully."), core::logging::LogType::YapmErr).await;
                            shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AddrNotAvailable => {
                            rclone.log(format!("Listener socket encountered a fatal address error: {e}. Shutting down."), core::logging::LogType::YapmErr).await;
                            shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        _ => {
                            rclone.log(format!("Unhandled listener accept error: {e}. Shutting down to be safe."), core::logging::LogType::YapmErr).await;
                            shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                }
            }
        }
    });
    unreachable!();
}

pub async fn handle_command<
    T: std::ops::Deref<Target = Device>,
    G: std::ops::Deref<Target = TGlobalAsyncIOManager>,
>(
    cmd: CLI,
    device: T,
    rt: G,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> CommandResult {
    match cmd.command {
        config::Command::Process(cmd) => handle_process_cmd(cmd, device.deref(), rt.deref()).await,
        config::Command::Config(cmd) => handle_config_cmd(cmd, device.deref(), rt.deref()).await,
        config::Command::Monitor(cmd) => handle_monitor_cmd(cmd, device.deref(), rt.deref()).await,
        config::Command::System(cmd) => {
            handle_system_cmd(cmd, device.deref(), rt.deref(), shutdown).await
        }
    }
}

pub async fn handle_process_cmd(
    cmd: config::ProcessCommands,
    device: &Device,
    rt: &TGlobalAsyncIOManager,
) -> CommandResult {
    match cmd {
        ProcessCommands::Start(args) => {
            let pcfg = ProcessConfig::from(args);
            return device.start(pcfg, rt).await;
        }
        ProcessCommands::Stop { process } => {
            return device.stop(process, rt).await;
        }
    }
}

pub async fn handle_config_cmd(
    cmd: config::ConfigCommands,
    device: &Device,
    rt: &TGlobalAsyncIOManager,
) -> CommandResult {
    match cmd {
        config::ConfigCommands::StartConfig { config } => {
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
            return device.start(contents, rt).await;
        }
    }
}

pub async fn handle_monitor_cmd(
    cmd: config::MonitorCommands,
    device: &Device,
    rt: &TGlobalAsyncIOManager,
) -> CommandResult {
    match cmd {
        config::MonitorCommands::List => device.list_processes().await,
        config::MonitorCommands::Show { process } => device.get_process_details(process).await,
        config::MonitorCommands::Log { lines } => CommandResult::Logs {
            vector: rt.get_logs(lines).await,
        },
        config::MonitorCommands::Metrics { process } => device.get_process_details(process).await,
    }
}

pub async fn handle_system_cmd(
    cmd: config::SystemCommands,
    device: &Device,
    rt: &TGlobalAsyncIOManager,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> CommandResult {
    match cmd {
        config::SystemCommands::Init { force } => {
            //this should be handled by the client, but if force, we want to shut down the daemon and restart it
            if force {
                device.stop_all().await;
                rt.kill_runtime().await;
                shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
                CommandResult::Success(format!("the runtimes have been terminated"))
            } else {
                //no-op
                CommandResult::Success(format!("The YAPM daemon has started successfully!"))
            }
        }
        config::SystemCommands::Status => {
            //maybe we can show YAPM stats here, i.e. how much RAM, etc
            CommandResult::Success(format!("yapmd is running"))
        }
        //we might have to handle these in another manner later
        config::SystemCommands::Shutdown => {
            if std::path::Path::new(DAEMON_SOCKET_ADDR).exists() {
                let _ = std::fs::remove_file(DAEMON_SOCKET_ADDR);
            }
            tracing::debug!("Does anything happen here?");
            device.stop_all().await;
            rt.kill_runtime().await;
            tracing::debug!("We've killed all the resources, is there a potential deadlock?");
            shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
            tracing::debug!(
                "Shutdown value after being set: {}",
                shutdown.load(std::sync::atomic::Ordering::SeqCst)
            );
            // std::process::exit(0);
            CommandResult::Success(format!("Shutdown process initiated, Daemon is terminating"))
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
    rt: &TGlobalAsyncIOManager,
) {
    //log failure when it happens, if it happens
    let s = match serde_json::to_string(response) {
        Ok(s) => s,
        Err(e) => {
            rt.log(
                format!("Serializing the response body failed: {e}"),
                core::logging::LogType::YapmErr,
            )
            .await;
            return;
        }
    };
    match stream.write_all(s.as_bytes()).await {
        Ok(_) => {}
        Err(e) => {
            rt.log(
                format!("Writing to the stream failed: {e}"),
                core::logging::LogType::YapmErr,
            )
            .await;
            return;
        }
    }

    let _ = stream.shutdown();
}

pub fn test_block(tex: &str) {
    let mut f = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .append(true)
        .open("tmptest.txt")
        .unwrap();
    writeln!(&mut f, "{}", tex).unwrap();
}

#[macro_export] //TODO: find a way to make this pub(crate)
macro_rules! resolve_os_declarations {
    ($linux_fn: stmt, $windows_fn: stmt) => {
        #[cfg(target_os = "linux")]
        $linux_fn;
        #[cfg(target_os = "windows")]
        $windows_fn;
    };
}
