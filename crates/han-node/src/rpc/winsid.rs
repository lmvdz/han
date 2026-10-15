//! The SID of the current user, from the process token.
#![allow(unsafe_code)]

use std::io;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PWSTR;

/// Closes the token handle on every path out.
struct Token(HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `OpenProcessToken` and is closed once, here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn err(e: windows::core::Error) -> io::Error {
    io::Error::other(format!("cannot tell the current user's SID: {e}"))
}

/// The string form (`S-1-5-21-...`) of the SID of the user this process runs as.
pub fn current_user_sid() -> io::Result<String> {
    let mut handle = HANDLE::default();
    // SAFETY: the pseudo handle of this process is always valid and `handle` is a live out parameter.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) }.map_err(err)?;
    let token = Token(handle);

    // The first call fails by design and says how long the TOKEN_USER is; u64 words keep the buffer aligned.
    let mut len = 0u32;
    // SAFETY: a null buffer of length 0 only asks for the needed length.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut len) };
    if len == 0 {
        return Err(io::Error::other("cannot tell the current user's SID"));
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` is at least `len` bytes and 8-aligned, which TOKEN_USER needs.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
    }
    .map_err(err)?;

    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points into the same buffer.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    // SAFETY: `sid` is valid; on success `text` is a LocalAlloc'd string that is freed once, below.
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(err)?;
    // SAFETY: `text` is a valid NUL-terminated string from the call above.
    let out = unsafe { text.to_string() };
    // SAFETY: the string was allocated by `ConvertSidToStringSidW` with LocalAlloc.
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    out.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_sid_is_a_user_sid_and_needs_no_child_process() {
        let sid = super::current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        assert!(sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    }
}
