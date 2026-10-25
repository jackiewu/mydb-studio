//! Asks the operating system to confirm the person at the computer is its
//! owner: Touch ID, or the login password, on macOS.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// This computer can't do it (another OS, or no password set).
    Unavailable(String),
    /// The person cancelled the prompt.
    Cancelled,
    Failed(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::Unavailable(m) => write!(f, "這台電腦不能使用系統驗證：{m}"),
            AuthError::Cancelled => write!(f, "已取消驗證"),
            AuthError::Failed(m) => write!(f, "驗證失敗：{m}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// Whether [`authenticate`] can be used here.
pub fn available() -> bool {
    imp::available().is_ok()
}

/// Shows the system prompt with `reason` and waits for the answer. Blocks,
/// so call it off the async runtime.
pub fn authenticate(reason: &str) -> Result<(), AuthError> {
    imp::authenticate(reason)
}

/// Calls `f` whenever the screen gets locked (including going to sleep with
/// a password required). Must be called on the main thread; a no-op where
/// the system doesn't tell.
pub fn watch_screen_lock(f: impl Fn() + Send + Sync + 'static) {
    imp::watch_screen_lock(f)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use std::ptr::NonNull;

    use objc2_foundation::{NSDistributedNotificationCenter, NSError, NSNotification, NSString};
    use objc2_local_authentication::{LAContext, LAError, LAPolicy};

    use super::AuthError;

    // Touch ID when there is one, else (or after failing) the login password.
    const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthentication;

    pub fn watch_screen_lock(f: impl Fn() + Send + Sync + 'static) {
        let center = NSDistributedNotificationCenter::defaultCenter();
        let name = NSString::from_str("com.apple.screenIsLocked");
        let block = RcBlock::new(move |_: NonNull<NSNotification>| f());
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(&name), None, None, &block)
        };
        // Observe for the life of the app.
        std::mem::forget(token);
    }

    pub fn available() -> Result<(), AuthError> {
        let ctx = unsafe { LAContext::new() };
        unsafe { ctx.canEvaluatePolicy_error(POLICY) }
            .map_err(|e| AuthError::Unavailable(e.localizedDescription().to_string()))
    }

    pub fn authenticate(reason: &str) -> Result<(), AuthError> {
        let ctx = unsafe { LAContext::new() };
        unsafe { ctx.canEvaluatePolicy_error(POLICY) }
            .map_err(|e| AuthError::Unavailable(e.localizedDescription().to_string()))?;
        let (tx, rx) = mpsc::channel::<Result<(), AuthError>>();
        let reply = RcBlock::new(move |ok: Bool, err: *mut NSError| {
            let result = if ok.as_bool() {
                Ok(())
            } else {
                // SAFETY: on failure the reply gets a valid NSError, or nil.
                match unsafe { err.as_ref() } {
                    Some(e)
                        if [
                            LAError::UserCancel.0,
                            LAError::SystemCancel.0,
                            LAError::AppCancel.0,
                        ]
                        .contains(&e.code()) =>
                    {
                        Err(AuthError::Cancelled)
                    }
                    Some(e) => Err(AuthError::Failed(e.localizedDescription().to_string())),
                    None => Err(AuthError::Failed("unknown error".into())),
                }
            };
            let _ = tx.send(result);
        });
        unsafe {
            ctx.evaluatePolicy_localizedReason_reply(POLICY, &NSString::from_str(reason), &reply)
        };
        rx.recv()
            .unwrap_or_else(|_| Err(AuthError::Failed("no answer from the system".into())))
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::AuthError;

    /// Debug builds may stand in an answer, for testing the lock screen
    /// where there is no system prompt: `MYDB_FAKE_SYSTEM_AUTH=ok` or `cancel`.
    fn fake() -> Option<String> {
        cfg!(debug_assertions)
            .then(|| std::env::var("MYDB_FAKE_SYSTEM_AUTH").ok())
            .flatten()
    }

    pub fn watch_screen_lock(_f: impl Fn() + Send + Sync + 'static) {}

    pub fn available() -> Result<(), AuthError> {
        match fake() {
            Some(_) => Ok(()),
            None => Err(AuthError::Unavailable("目前只支援 macOS".into())),
        }
    }

    pub fn authenticate(_reason: &str) -> Result<(), AuthError> {
        match fake().as_deref() {
            Some("ok") => Ok(()),
            Some(_) => Err(AuthError::Cancelled),
            None => Err(AuthError::Unavailable("目前只支援 macOS".into())),
        }
    }
}
