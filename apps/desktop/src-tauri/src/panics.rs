//! Panics as errors. The release build unwinds (`panic = "unwind"`), so a worker thread can turn a panic in a
//! library (xcap's graphics setup, say) into an error message instead of the whole app ending.
//!
//! `catch_unwind` does not cross FFI: a panic inside a callback that C code calls (cpal's audio callback on
//! CoreAudio) still aborts the process (Rust 1.81+ guarantees an abort, not undefined behaviour, at `extern "C"`
//! boundaries). Such callbacks are kept free of panicking code.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// The longest panic message kept for the log.
const MAX_MESSAGE: usize = 300;

/// The text of a panic payload (`panic!("...")` gives a `&str` or a `String`), shortened for the log.
pub fn message(payload: &(dyn Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_string());
    if text.chars().count() > MAX_MESSAGE {
        text.chars().take(MAX_MESSAGE).chain(std::iter::once('…')).collect()
    } else {
        text
    }
}

/// Run `f`; a panic inside it is logged (`what` and the panic's message only) and becomes `Err(user_message)`.
pub fn guarded<T>(what: &str, user_message: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            eprintln!("[pawl] {what} failed: {}", message(&*payload));
            Err(user_message.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_becomes_the_user_message() {
        assert_eq!(guarded("test", "Not available.", || Ok::<_, String>(5)), Ok(5));
        assert_eq!(guarded("test", "Not available.", || Err::<(), _>("its own error".to_string())), Err("its own error".into()));
        let r: Result<(), String> = guarded("test", "Not available.", || panic!("Failed to create D3D11 device"));
        assert_eq!(r, Err("Not available.".into()));
        let r: Result<(), String> = guarded("test", "Not available.", || panic!("{} {}", "formatted", 42));
        assert_eq!(r, Err("Not available.".into()));
    }

    #[test]
    fn messages_are_read_and_shortened() {
        let p = catch_unwind(|| panic!("static text")).unwrap_err();
        assert_eq!(message(&*p), "static text");
        let p = catch_unwind(|| panic!("{}", "x".repeat(1000))).unwrap_err();
        assert_eq!(message(&*p).chars().count(), MAX_MESSAGE + 1);
        let p = catch_unwind(|| std::panic::panic_any(7u8)).unwrap_err();
        assert_eq!(message(&*p), "a panic without a message");
    }
}
