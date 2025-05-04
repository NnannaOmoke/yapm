#![allow(dead_code)]
#![allow(unused_variables)]

//TODO: there needs to be a parked thread to reap processes
//Solution: create a task.rs to manage these thread managers
//TODO: we need to be able to collect input from child processes
//Solution: This is something we have to test and work on now, probably by finding non-blocking ways to collect process input
//TODO: probably use async for the more I/O intensive read-writes for logging
//Solution: get the crates: aysnc-fs, pollster, futures, but this will all be done in the logging.rs and threads.rs
//TODO: we now want to look at ways to design permissions for processes
//this would involve re-writing some functions from the Nix crate

mod config;
mod core;
mod threads;

fn main() {}

#[macro_export] //TODO: find a way to make this pub(crate)
macro_rules! resolve_os_declarations {
    ($linux_fn: stmt, $windows_fn: stmt) => {
        #[cfg(target_os = "linux")]
        $linux_fn;
        #[cfg(target_os = "windows")]
        $windows_fn;
    };
}
