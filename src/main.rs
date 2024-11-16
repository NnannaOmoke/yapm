#![allow(dead_code)]
#![allow(unused_variables)]

mod core;

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
