//! A node started through `conhost.exe --headless` (the Windows logon task) ends with that console host.
//!
//! Stopping the task ends `conhost.exe` but not the program it started, so the node would stay up holding its
//! data directory, and the next start would be a second node on the same data. The node therefore watches its
//! parent: when the parent is `conhost.exe`, the node exits as soon as that process ends. A node started from a
//! shell, a service or a test has another parent and is not affected.
#![allow(unsafe_code)]

/// Makes the node exit when its parent `conhost.exe` ends. Does nothing on other systems or other parents.
pub fn die_with_console_host() {
    #[cfg(windows)]
    imp::watch();
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    /// The processes now running as `(pid, parent pid, image name)`.
    fn processes() -> Vec<(u32, u32, String)> {
        // SAFETY: a snapshot handle is closed once below; the entry is a live out parameter with its size set.
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return Vec::new();
            };
            if snap == INVALID_HANDLE_VALUE {
                return Vec::new();
            }
            let mut out = Vec::new();
            let mut e = PROCESSENTRY32W {
                dwSize: size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut more = Process32FirstW(snap, &mut e).is_ok();
            while more {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(0);
                out.push((
                    e.th32ProcessID,
                    e.th32ParentProcessID,
                    String::from_utf16_lossy(&e.szExeFile[..len]),
                ));
                more = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
            out
        }
    }

    pub fn watch() {
        let me = unsafe { GetCurrentProcessId() };
        let all = processes();
        let Some(parent) = all.iter().find(|p| p.0 == me).map(|p| p.1) else {
            return;
        };
        if !all
            .iter()
            .any(|p| p.0 == parent && p.2.eq_ignore_ascii_case("conhost.exe"))
        {
            return;
        }
        // SAFETY: the handle is a fresh one for the parent, owned by the thread below.
        let Ok(h) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent) }) else {
            return;
        };
        let h = h.0 as usize;
        let _ = std::thread::Builder::new()
            .name("console-host-watch".into())
            .spawn(move || {
                let h = HANDLE(h as *mut _);
                // SAFETY: `h` is a valid process handle until it is closed here.
                unsafe {
                    WaitForSingleObject(h, INFINITE);
                    let _ = CloseHandle(h);
                }
                std::process::exit(0);
            });
    }
}
