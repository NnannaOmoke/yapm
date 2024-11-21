use std::sync::mpsc::channel;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;

fn io_executor<T>(task: Receiver<T>) {
    loop {}
}
