//! Runs file system work as the signed-in user. The service's agent runs as SYSTEM, which may open any file on
//! the machine; what a remote viewer browses, downloads or uploads through the file browser must stay within what
//! the user at this computer may touch. On Windows, under SYSTEM, the calling thread impersonates the user of the
//! agent's session (`WTSQueryUserToken`) for the duration of `f`; everywhere else `f` simply runs (the process
//! already is that user).

#[cfg(windows)]
/// Why work could not run as the user.
pub const NO_USER: &str = "No user is signed in on this computer";

/// Runs `f` as the signed-in user; `Err(NO_USER)` when there is none to act as (SYSTEM with nobody signed in).
pub fn as_user<T>(f: impl FnOnce() -> T) -> Result<T, &'static str> {
    #[cfg(windows)]
    if crate::infra::system_service::is_local_system() {
        return win::impersonated(f);
    }
    Ok(f())
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::ptr;

    type Handle = *mut c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    #[link(name = "wtsapi32")]
    unsafe extern "system" {
        fn WTSQueryUserToken(session_id: u32, token: *mut Handle) -> i32;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ImpersonateLoggedOnUser(token: Handle) -> i32;
        fn RevertToSelf() -> i32;
    }

    /// Closes the token and ends the impersonation even if `f` panics.
    struct Scope(Handle);

    impl Drop for Scope {
        fn drop(&mut self) {
            unsafe {
                RevertToSelf();
                CloseHandle(self.0);
            }
        }
    }

    pub fn impersonated<T>(f: impl FnOnce() -> T) -> Result<T, &'static str> {
        let mut session = 0u32;
        let mut token: Handle = ptr::null_mut();
        // SAFETY: plain Win32 calls with valid out pointers; the token is closed by `Scope`.
        unsafe {
            if ProcessIdToSessionId(GetCurrentProcessId(), &mut session) == 0
                || WTSQueryUserToken(session, &mut token) == 0
            {
                return Err(super::NO_USER);
            }
            if ImpersonateLoggedOnUser(token) == 0 {
                CloseHandle(token);
                return Err(super::NO_USER);
            }
        }
        let _scope = Scope(token);
        Ok(f())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn outside_the_service_work_runs_as_is() {
        // Tests never run as SYSTEM.
        assert_eq!(super::as_user(|| 41 + 1), Ok(42));
    }
}
