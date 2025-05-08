use std::mem::zeroed;
use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HANDLE};
use windows_sys::Win32::System::Threading::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
};

pub fn limit_process_memory(handle: HANDLE, limit_in_bytes: usize) -> Result<(), String> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job == 0 {
            return Err("Failed to create Job Object".into());
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        info.ProcessMemoryLimit = limit_in_bytes;

        let success: BOOL = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            //demonic pointer cast, because windows...
            &mut info as *mut _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );

        if success == 0 {
            CloseHandle(job);
            return Err("Failed to set Job Object memory limit".into());
        }

        let success: BOOL = AssignProcessToJobObject(job, handle);
        if success == 0 {
            CloseHandle(job);
            return Err("Failed to assign process to Job Object".into());
        }

        Ok(())
    }
}
use std::ffi::OsString;
use std::ptr;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW};

#[cfg(target_os = "windows")]
pub fn spawn_process(app_name: &str, args: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStringExt;
    let app_name_wide: Vec<u16> = OsString::from(app_name)
        .ne()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut cmd_line = OsString::from(app_name);
    cmd_line.push(" ");
    cmd_line.push(args);
    let cmd_line_wide: Vec<u16> = cmd_line.encode_wide().chain(std::iter::once(0)).collect();

    let mut startup_info: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup_info.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let success = unsafe {
        CreateProcessW(
            app_name_wide.as_ptr(),
            cmd_line_wide.as_ptr() as *mut _,
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut startup_info,
            &mut process_info,
        )
    };

    if success == 0 {
        return Err(format!("Failed to spawn process: {}", app_name));
    }

    Ok(())
}

use std::time::SystemTime;

#[derive(Debug)]
pub struct ProcessTimes {
    pub creation_time: SystemTime,
    pub exit_time: Option<SystemTime>,
    pub kernel_time: u64,
    pub user_time: u64,
}

use std::time::SystemTime;
use windows_sys::Win32::Foundation::{GetLastError, FILETIME, HANDLE};
use windows_sys::Win32::System::Threading::GetProcessTimes;

pub fn get_process_times(handle: HANDLE) -> Result<ProcessTimes, String> {
    let mut creation_time: FILETIME = unsafe { std::mem::zeroed() };
    let mut exit_time: FILETIME = unsafe { std::mem::zeroed() };
    let mut kernel_time: FILETIME = unsafe { std::mem::zeroed() };
    let mut user_time: FILETIME = unsafe { std::mem::zeroed() };

    let success = unsafe {
        GetProcessTimes(
            handle,
            &mut creation_time,
            &mut exit_time,
            &mut kernel_time,
            &mut user_time,
        )
    };

    if success == 0 {
        let error_code = unsafe { GetLastError() };
        return Err(format!(
            "Failed to get process times: Error code {}",
            error_code
        ));
    }

    let filetime_to_u64 =
        |ft: &FILETIME| ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64);

    let filetime_to_systemtime = |ft: &FILETIME| -> Option<SystemTime> {
        let filetime_u64 = filetime_to_u64(ft);
        if filetime_u64 == 0 {
            return None;
        }
        let duration_since_1601 = std::time::Duration::from_nanos(filetime_u64 * 100);
        let epoch_1601 =
            SystemTime::UNIX_EPOCH.checked_sub(std::time::Duration::from_secs(11644473600))?; // 1601 to 1970 offset
        epoch_1601.checked_add(duration_since_1601)
    };

    let creation_system_time =
        filetime_to_systemtime(&creation_time).ok_or("Failed to convert creation time")?;
    let exit_system_time = filetime_to_systemtime(&exit_time); // May be None if process hasn't exited

    Ok(ProcessTimes {
        creation_time: creation_system_time,
        exit_time: exit_system_time,
        kernel_time: filetime_to_u64(&kernel_time),
        user_time: filetime_to_u64(&user_time),
    })
}
