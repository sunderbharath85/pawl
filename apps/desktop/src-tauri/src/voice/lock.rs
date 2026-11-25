//! Is the screen locked? Best effort per OS, polled by the voice worker while voice is on (the microphone is
//! released while locked). `None` = the OS did not say; the mic then follows the other rules.
//!
//! - Windows: the input desktop cannot be opened while the lock screen (Winlogon desktop) is up.
//!   https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-openinputdesktop
//! - macOS: `CGSSessionScreenIsLocked` in the Quartz session dictionary.
//!   https://developer.apple.com/documentation/coregraphics/1454556-cgsessioncopycurrentdictionary
//! - Linux: systemd-logind's `LockedHint` for this session (set by desktops that report locking).
//!   https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html

use std::time::Duration;

/// How often the worker asks.
#[cfg(target_os = "linux")]
pub const POLL_EVERY: Duration = Duration::from_secs(3);
#[cfg(not(target_os = "linux"))]
pub const POLL_EVERY: Duration = Duration::from_secs(1);

#[cfg(windows)]
pub fn screen_locked() -> Option<bool> {
    use std::ffi::c_void;
    #[link(name = "user32")]
    extern "system" {
        fn OpenInputDesktop(flags: u32, inherit: i32, desired_access: u32) -> *mut c_void;
        fn CloseDesktop(desktop: *mut c_void) -> i32;
    }
    const DESKTOP_SWITCHDESKTOP: u32 = 0x0100;
    // SAFETY: plain Win32 calls; the handle is closed right away.
    unsafe {
        let h = OpenInputDesktop(0, 0, DESKTOP_SWITCHDESKTOP);
        if h.is_null() {
            Some(true)
        } else {
            CloseDesktop(h);
            Some(false)
        }
    }
}

#[cfg(target_os = "macos")]
pub fn screen_locked() -> Option<bool> {
    use std::ffi::{c_char, c_void};
    type CFTypeRef = *const c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGSessionCopyCurrentDictionary() -> CFTypeRef;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(alloc: CFTypeRef, c_str: *const c_char, encoding: u32) -> CFTypeRef;
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(boolean: CFTypeRef) -> u8;
        fn CFRelease(cf: CFTypeRef);
    }
    const UTF8: u32 = 0x0800_0100;
    // SAFETY: Core Foundation "Copy"/"Create" results are released here; the dictionary value is borrowed.
    unsafe {
        let dict = CGSessionCopyCurrentDictionary();
        if dict.is_null() {
            return None;
        }
        let key = CFStringCreateWithCString(std::ptr::null(), c"CGSSessionScreenIsLocked".as_ptr(), UTF8);
        if key.is_null() {
            CFRelease(dict);
            return None;
        }
        let v = CFDictionaryGetValue(dict, key);
        let locked = !v.is_null() && CFGetTypeID(v) == CFBooleanGetTypeID() && CFBooleanGetValue(v) != 0;
        CFRelease(key);
        CFRelease(dict);
        Some(locked)
    }
}

#[cfg(target_os = "linux")]
pub fn screen_locked() -> Option<bool> {
    let session = std::env::var("XDG_SESSION_ID").ok().filter(|s| !s.is_empty() && s.len() < 64);
    let session = session.as_deref().unwrap_or("auto");
    let out = std::process::Command::new("loginctl")
        .args(["show-session", session, "-p", "LockedHint", "--value"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn screen_locked() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn answers_without_panicking() {
        // No assertion: a CI service session has no interactive desktop and may read as locked.
        eprintln!("screen locked: {:?}", super::screen_locked());
    }
}
