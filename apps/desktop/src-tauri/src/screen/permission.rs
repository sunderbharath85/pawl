//! macOS: the Screen Recording permission (System Settings > Privacy & Security > Screen & System Audio Recording;
//! "Screen Recording" before macOS 15). Without it a capture shows only the desktop picture and Pawl's own windows,
//! and windows have no titles, so Pawl asks first and says what to do. No Info.plist key or entitlement is involved
//! (Pawl is not sandboxed). Other systems need nothing here.

/// What the user is told when the permission is missing.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const MISSING: &str = "Pawl needs permission to see your screen. Turn on Pawl in System Settings > Privacy & \
Security > Screen & System Audio Recording (Screen Recording on macOS 14), then quit and reopen Pawl.";

#[cfg(target_os = "macos")]
mod mac {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        /// Whether the app may capture the screen; never shows anything (macOS 10.15+).
        pub fn CGPreflightScreenCaptureAccess() -> bool;
        /// Asks once: the system's "Pawl would like to record this computer's screen" alert, which leads to
        /// System Settings. Later calls show nothing and return the current answer.
        pub fn CGRequestScreenCaptureAccess() -> bool;
    }
}

/// `Ok` when Pawl may capture the screen. On macOS without the permission it asks the system (the alert appears
/// the first time) and returns `MISSING`: the grant only takes effect after Pawl restarts.
pub fn ensure() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: plain CoreGraphics calls without arguments.
        if unsafe { mac::CGPreflightScreenCaptureAccess() } {
            return Ok(());
        }
        if unsafe { mac::CGRequestScreenCaptureAccess() } {
            return Ok(());
        }
        Err(MISSING.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}
