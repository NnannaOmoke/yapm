use std::collections::VecDeque;

use std::fmt::Debug;

use std::fs::File;

use std::io::Write;

use std::process::ChildStderr;
use std::process::ChildStdout;

use std::sync::Arc;
use std::sync::Mutex;

use thiserror::Error;

use time::error::IndeterminateOffset;
use time::OffsetDateTime;

use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;

use tokio::process::ChildStderr as TokioStderr;
use tokio::process::ChildStdout as TokioStdout;

use tokio::runtime::Builder;
use tokio::runtime::Runtime;

use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

pub enum AsyncTask {
    StreamIOToFile(ChildStderr, ChildStdout, File, File, i32),
    KillThread,
}

#[derive(Debug, Error)]
pub enum AsyncError {
    #[error("Unknown Error")]
    UnknownAsyncError,
    #[error("I/O error: {}", .0)]
    AysncIOError(#[from] std::io::Error),
    #[error("Time init error: {}", .0)]
    TimeError(#[from] IndeterminateOffset),
}

type AsyncResult<T> = Result<T, AsyncError>;

pub struct GlobalAysncIOManager {
    rt: Runtime,
    sender: UnboundedSender<AsyncTask>,
}

impl GlobalAysncIOManager {
    pub fn new() -> AsyncResult<Self> {
        let (tx, rx) = unbounded_channel();
        let rt = Builder::new_current_thread()
            .worker_threads(4)
            .thread_name("YAPM async handler")
            .build()?;
        rt.spawn(Self::coordinator(rx));
        Ok(Self { sender: tx, rt })
    }

    pub fn give_sender(&self) -> UnboundedSender<AsyncTask> {
        self.sender.clone()
    }

    pub async fn coordinator(mut rx: UnboundedReceiver<AsyncTask>) -> AsyncResult<()> {
        // let mut tasks = vec![];
        while let Some(msg) = rx.recv().await {
            match msg {
                AsyncTask::StreamIOToFile(stderr, stdout, ferr, fout, pid) => {}
                AsyncTask::KillThread => {}
            }
        }
        Ok(())
    }

    pub async fn stream_to_file(
        stderr: ChildStderr,
        stdout: ChildStdout,
        mut ferr: File,
        mut fout: File,
        global_log_vector: Arc<Mutex<VecDeque<String>>>,
        pid: i32,
    ) -> AsyncResult<()> {
        let stderr = TokioStderr::from_std(stderr)?;
        let stdout = TokioStdout::from_std(stdout)?;

        let mut err_reader = BufReader::new(stderr).lines();
        let mut out_reader = BufReader::new(stdout).lines();

        while let Some(line) = err_reader.next_line().await? {
            //we need to get the time crate so that we can format this properly
            let time = OffsetDateTime::now_local()?;
            let complete = format! {"[{}:{}:{}:{}:{}:{}] {}\n", time.year(), time.month(), time.day(), time.hour(), time.minute(), time.millisecond(), line};
            ferr.write(complete.as_bytes())?;
            ferr.flush()?;
            let curr = global_log_vector.clone();
            let lock = curr.lock();
            if let Ok(mut lk) = lock {
                if lk.len() >= 10 {
                    lk.pop_front();
                    lk.push_back(complete);
                } else {
                    lk.push_back(complete)
                }
            }
        }

        while let Some(line) = out_reader.next_line().await? {
            let time = OffsetDateTime::now_local()?;
            let complete = format! {"[{}:{}:{}:{}:{}:{}] {}\n", time.year(), time.month(), time.day(), time.hour(), time.minute(), time.millisecond(), line};
            fout.write(complete.as_bytes())?;
            fout.flush()?;
            let curr = global_log_vector.clone();
            let lock = curr.lock();
            if let Ok(mut lk) = lock {
                if lk.len() >= 10 {
                    lk.pop_front();
                    lk.push_back(complete);
                } else {
                    lk.push_back(complete)
                }
            }
        }

        Ok(())
    }
}
