//! The push-to-talk shortcut: "Ctrl+Alt+Space" style strings (the UI's `parseHotkey` form) checked here with the
//! same rules, then parsed into a global shortcut. Press starts an utterance, release ends it.

use std::str::FromStr;
use tauri_plugin_global_shortcut::Shortcut;

const MODS: [&str; 4] = ["ctrl", "alt", "shift", "super"];

fn is_function_key(k: &str) -> bool {
    k.strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n))
}

fn is_allowed_key(k: &str) -> bool {
    let single = k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric());
    single
        || is_function_key(k)
        || matches!(
            k,
            "space" | "enter" | "tab" | "backspace" | "up" | "down" | "left" | "right" | "home" | "end" | "pageup"
                | "pagedown" | "insert" | "delete"
        )
}

/// The shortcut for `text`, or why it can't be one. A key needs Ctrl, Alt or Super with it (a plain key, or one with
/// only Shift, would fire while the user types), except F1-F24.
pub fn parse(text: &str) -> Result<Shortcut, String> {
    let parts: Vec<String> = text.split('+').map(|p| p.trim().to_ascii_lowercase()).collect();
    if parts.iter().any(String::is_empty) {
        return Err(format!("“{text}” is not a key combination."));
    }
    let mut key: Option<&str> = None;
    let mut strong_mod = false;
    for p in &parts {
        if MODS.contains(&p.as_str()) {
            strong_mod |= p != "shift";
            continue;
        }
        if key.is_some() {
            return Err("Use one key with Ctrl, Alt, Shift or Win.".into());
        }
        if !is_allowed_key(p) {
            return Err(format!("“{p}” can't be part of a shortcut."));
        }
        key = Some(p);
    }
    let key = key.ok_or("Add a key after the modifiers (for example Ctrl+Alt+Space).")?;
    if !strong_mod && !is_function_key(key) {
        return Err("Add Ctrl, Alt or Win: a key on its own (or with only Shift) would go off while you type.".into());
    }
    Shortcut::from_str(text.trim()).map_err(|e| format!("“{text}” is not a key combination: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri_plugin_global_shortcut::{Code, Modifiers};

    #[test]
    fn accepts_the_ui_forms() {
        let s = parse("Ctrl+Alt+Space").unwrap();
        assert_eq!(s, Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space));
        let s = parse("Super+Shift+K").unwrap();
        assert_eq!(s, Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyK));
        assert_eq!(parse("F9").unwrap(), Shortcut::new(None, Code::F9));
        assert_eq!(parse("Alt+7").unwrap(), Shortcut::new(Some(Modifiers::ALT), Code::Digit7));
        assert_eq!(parse("ctrl + up").unwrap(), Shortcut::new(Some(Modifiers::CONTROL), Code::ArrowUp));
    }

    #[test]
    fn refuses_keys_that_would_fire_while_typing() {
        for bad in ["Space", "A", "Shift+A", "Ctrl+Alt", "Ctrl+A+B", "Ctrl++A", "", "Ctrl+Esc", "Ctrl+F25", "Ctrl+ä"] {
            assert!(parse(bad).is_err(), "{bad:?} should be refused");
        }
    }
}
