#![allow(dead_code)]
#![allow(unused_variables)]

use utils::entry;

mod config;
mod core;
mod protocol;
mod threads;
mod utils;

const DAEMON_SOCKET_ADDR: &'static str = "/tmp/yapmd.sock";

pub fn main() {
    entry()
}

pub fn test_block(tex: &str) {
    use std::io::Write;
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
