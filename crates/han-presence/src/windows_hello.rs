//! Windows: Windows Hello through WinRT `Windows.Security.Credentials.UI.UserConsentVerifier`.
//!
//! The message of the request is the text Windows shows in the Hello dialog (the action and the short plan
//! digest). Hello offers face, fingerprint or PIN, whichever the person set up.
//!
//! The dialog needs a real top-level owner window to appear. `RequestVerificationAsync` takes the console
//! window of the process, and a node started under Windows Terminal's pseudo console, through
//! `conhost.exe --headless` or hidden has none, so Windows shows nothing and the request ends in a timeout.
//! The request therefore goes through `IUserConsentVerifierInterop::RequestVerificationForWindowAsync` with a
//! small window of the node's own, brought to the foreground for the prompt and destroyed after it.
#![allow(unsafe_code)]

use std::time::{Duration, Instant};

use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::{
    IUserConsentVerifierInterop, RO_INIT_MULTITHREADED, RoInitialize,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_MENU, keybd_event};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics, PM_REMOVE,
    PeekMessageW, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, SetForegroundWindow,
    ShowWindow, TranslateMessage, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{HSTRING, w};
use windows_future::{AsyncStatus, IAsyncOperation};

use crate::{Presence, PresenceError, PresencePrompt, PresenceProof};

/// Windows Hello.
pub struct WindowsHello;

/// Whether Hello can verify anyone on this device, without showing anything.
pub fn availability() -> Result<UserConsentVerifierAvailability, PresenceError> {
    let op = UserConsentVerifier::CheckAvailabilityAsync()
        .map_err(|e| PresenceError::Unavailable(format!("Windows Hello: {e}")))?;
    wait(&op, Duration::from_secs(10))?
        .map_err(|e| PresenceError::Unavailable(format!("Windows Hello: {e}")))
}

/// Polls `op` until it ends or `timeout` passes. On a timeout the operation is cancelled (which closes the
/// dialog) and the wait is `Err(TimedOut)`; otherwise the operation's own result is the inner one.
fn wait<T: windows::core::RuntimeType + 'static>(
    op: &IAsyncOperation<T>,
    timeout: Duration,
) -> Result<windows::core::Result<T>, PresenceError> {
    wait_pumping(op, timeout, || {})
}

/// [`wait`], calling `pump` between polls (a window's messages are handled on the thread that made it).
fn wait_pumping<T: windows::core::RuntimeType + 'static>(
    op: &IAsyncOperation<T>,
    timeout: Duration,
    mut pump: impl FnMut(),
) -> Result<windows::core::Result<T>, PresenceError> {
    let deadline = Instant::now() + timeout;
    loop {
        match op.Status() {
            Ok(AsyncStatus::Started) => {}
            Ok(AsyncStatus::Completed) => return Ok(op.GetResults()),
            Ok(_) => return Ok(op.GetResults()),
            Err(e) => return Ok(Err(e)),
        }
        if Instant::now() >= deadline {
            let _ = op.Cancel();
            return Err(PresenceError::TimedOut);
        }
        pump();
        std::thread::sleep(Duration::from_millis(50));
    }
}

unsafe extern "system" fn owner_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: the arguments are the ones Windows gave this window procedure.
    unsafe { DefWindowProcW(h, m, w, l) }
}

/// The owner window of the prompt, destroyed on drop.
struct Owner(HWND);

impl Owner {
    /// A small top-level window of this thread, shown and brought to the foreground.
    fn new() -> Result<Self, PresenceError> {
        let fail = |e: windows::core::Error| {
            PresenceError::Unavailable(format!("Windows Hello owner window: {e}"))
        };
        // SAFETY: plain Win32 calls with a class and window made here; the window is destroyed once, on drop.
        unsafe {
            let instance = GetModuleHandleW(None).map_err(fail)?;
            let class = w!("HanPresenceOwner");
            // A second registration of the class fails, which is fine.
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(owner_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
            let (x, y) = (
                GetSystemMetrics(SM_CXSCREEN) / 2,
                GetSystemMetrics(SM_CYSCREEN) / 2,
            );
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                class,
                w!("HAN approval"),
                WS_POPUP,
                x,
                y,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(fail)?;
            let _ = ShowWindow(hwnd, SW_SHOW);
            // Windows lets a process take the foreground only after input; a tap of Alt counts.
            keybd_event(VK_MENU.0 as u8, 0, Default::default(), 0);
            keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
            let _ = SetForegroundWindow(hwnd);
            Ok(Owner(hwnd))
        }
    }

    /// Handles the window's pending messages.
    fn pump(&self) {
        let mut msg = Default::default();
        // SAFETY: `msg` is a live out parameter and the messages are those of this thread's window.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        // SAFETY: the window belongs to this thread and is destroyed once.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

/// Asks Hello for a confirmation with the text `message`, owned by a window of its own, within `timeout`.
/// Everything runs on one thread, which owns the window and handles its messages.
fn request(
    message: &str,
    timeout: Duration,
) -> Result<UserConsentVerificationResult, PresenceError> {
    let message = message.to_owned();
    std::thread::Builder::new()
        .name("windows-hello".into())
        .spawn(move || {
            let fail =
                |e: windows::core::Error| PresenceError::Unavailable(format!("Windows Hello: {e}"));
            // SAFETY: initialises the thread for WinRT; it may already be, which is fine.
            let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
            let owner = Owner::new()?;
            let interop =
                windows::core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>()
                    .map_err(fail)?;
            // SAFETY: `owner.0` is a live window of this thread and the message an owned string.
            let op: IAsyncOperation<UserConsentVerificationResult> = unsafe {
                interop.RequestVerificationForWindowAsync(owner.0, &HSTRING::from(message))
            }
            .map_err(fail)?;
            wait_pumping(&op, timeout, || owner.pump())?.map_err(fail)
        })
        .map_err(|e| PresenceError::Unavailable(format!("Windows Hello: {e}")))?
        .join()
        .unwrap_or_else(|_| {
            Err(PresenceError::Unavailable(
                "Windows Hello: its thread failed".into(),
            ))
        })
}

fn unavailable(a: UserConsentVerifierAvailability) -> Option<&'static str> {
    if a == UserConsentVerifierAvailability::Available {
        None
    } else if a == UserConsentVerifierAvailability::DeviceNotPresent {
        Some(
            "this device has no Windows Hello hardware (no camera, fingerprint reader or PIN support)",
        )
    } else if a == UserConsentVerifierAvailability::NotConfiguredForUser {
        Some(
            "Windows Hello is not set up for this user; set up a PIN or biometrics in Settings > Accounts",
        )
    } else if a == UserConsentVerifierAvailability::DisabledByPolicy {
        Some("Windows Hello is disabled by policy on this device")
    } else {
        Some("Windows Hello is busy or unavailable")
    }
}

fn outcome(r: UserConsentVerificationResult) -> Result<(), PresenceError> {
    if r == UserConsentVerificationResult::Verified {
        Ok(())
    } else if r == UserConsentVerificationResult::Canceled {
        Err(PresenceError::Cancelled)
    } else if r == UserConsentVerificationResult::RetriesExhausted {
        Err(PresenceError::Denied("too many failed attempts".into()))
    } else if r == UserConsentVerificationResult::DeviceNotPresent
        || r == UserConsentVerificationResult::NotConfiguredForUser
        || r == UserConsentVerificationResult::DisabledByPolicy
    {
        Err(PresenceError::Unavailable(
            "Windows Hello is not usable on this device".into(),
        ))
    } else {
        Err(PresenceError::Denied(
            "Windows Hello is busy; try again".into(),
        ))
    }
}

impl Presence for WindowsHello {
    fn name(&self) -> &'static str {
        "windows-hello"
    }

    fn confirm(&self, prompt: &PresencePrompt) -> Result<PresenceProof, PresenceError> {
        if let Some(why) = unavailable(availability()?) {
            return Err(PresenceError::Unavailable(why.into()));
        }
        outcome(request(&prompt.message(), prompt.timeout)?)?;
        Ok(PresenceProof::new(
            "windows-hello",
            &prompt.plan_digest_short,
        ))
    }
}
