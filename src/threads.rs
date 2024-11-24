use std::fs::File;

use std::process::ChildStderr;
use std::process::ChildStdout;

use std::sync::mpsc::channel;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::Arc;

pub enum AsyncTask {
    StreamIOToFile(ChildStderr, ChildStdout, File, File),
    KillThread,
}
