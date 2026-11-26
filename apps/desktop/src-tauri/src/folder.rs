//! `open_folder`: show a folder in the system's file manager (Settings > Assistant, "Open folder").
//!
//! The path must be absolute and name an existing folder (after `canonicalize`, so `..` and links are resolved).
//! The file manager is started directly with the folder as its one argument, never through a shell, and Pawl does
//! not wait for it. On Windows only folders on this computer's drives are accepted: a `\\server\share` path would
//! make Windows connect to that server (and sign in with the user's account) while just checking the path. The
//! same holds for every symlink or junction on the way: their targets are read, without following them, before
//! anything opens the path, and the canonical result is checked again (a mapped network drive comes back as its
//! `\\server\share` path, so it is refused too).
//!
//! Nothing in the folder is run:
//! - macOS: an app is a folder too (`Foo.app`), and `open Foo.app` launches it. Finder is asked to reveal the
//!   folder instead (`open -R`), which never launches anything, and app-like bundles are refused outright.
//! - Linux: `xdg-open` hands a directory (MIME type `inode/directory`) to the user's file manager; a launcher
//!   (`.desktop`) is a file, refused above, also when reached through a link.
//! - Windows: Explorer only shows a folder. A `desktop.ini` in it (read only for system or read-only folders) sets
//!   its icon, name and tip, and can name a shell extension only if that is already installed.
//!
//! - std::fs::canonicalize: https://doc.rust-lang.org/std/fs/fn.canonicalize.html
//! - std::process::Command: https://doc.rust-lang.org/std/process/struct.Command.html
//! - xdg-open: https://portland.freedesktop.org/doc/xdg-open.html
//! - open(1) `-R`: https://ss64.com/mac/open.html (`man open`)
//! - desktop.ini: https://learn.microsoft.com/windows/win32/shell/how-to-customize-folders-with-desktop-ini

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const NOT_ABSOLUTE: &str = "Use the folder's full path.";
const MISSING: &str = "That folder doesn't exist.";
const UNREADABLE: &str = "Pawl can't read that folder.";
const NOT_A_FOLDER: &str = "That's a file, not a folder.";
const AN_APP: &str = "That's an app, not a folder.";
#[cfg(windows)]
const NOT_LOCAL: &str = "Use a folder on this computer.";
const NO_FILE_MANAGER: &str = "Couldn't open the file manager.";

/// Show `path` (an absolute path to an existing folder) in Explorer, Finder or the desktop's file manager.
#[tauri::command]
pub async fn open_folder(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = resolve(&path)?;
        show(&dir)
    })
    .await
    .map_err(|_| NO_FILE_MANAGER.to_string())?
}

/// Bundles macOS treats as an app or as code rather than a folder (matched case-insensitively).
const BUNDLE_EXTENSIONS: [&str; 6] = ["app", "bundle", "framework", "plugin", "prefpane", "kext"];

/// The folder the file manager gets: absolute, canonical, existing, a directory (on macOS, not a bundle).
fn resolve(path: &str) -> Result<PathBuf, String> {
    resolve_with(path, cfg!(target_os = "macos"))
}

fn resolve_with(path: &str, refuse_bundles: bool) -> Result<PathBuf, String> {
    let given = Path::new(path);
    if path.trim().is_empty() || !given.is_absolute() {
        return Err(NOT_ABSOLUTE.into());
    }
    #[cfg(windows)]
    let given = &checked_local(given)?;
    let dir = std::fs::canonicalize(given).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => MISSING.to_string(),
        _ => UNREADABLE.to_string(),
    })?;
    // A mapped network drive (Windows reports it as `\\?\UNC\…`), or a link changed since the walk.
    #[cfg(windows)]
    if !on_a_local_drive(&dir) {
        return Err(NOT_LOCAL.into());
    }
    let meta = std::fs::metadata(&dir).map_err(|_| UNREADABLE.to_string())?;
    if !meta.is_dir() {
        return Err(NOT_A_FOLDER.into());
    }
    // The canonical name, so a link called `Notes` that points at `Evil.app` is caught too.
    if refuse_bundles && is_bundle(&dir) {
        return Err(AN_APP.into());
    }
    Ok(for_display(dir))
}

/// Does the last component end in one of [`BUNDLE_EXTENSIONS`]?
fn is_bundle(dir: &Path) -> bool {
    dir.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| BUNDLE_EXTENSIONS.contains(&e.as_str()))
}

/// `C:\…` or `\\?\C:\…`, not a `\\server\share`, `\\?\UNC\…`, `\\?\GLOBALROOT\…` or `\\.\device` path.
#[cfg(windows)]
fn on_a_local_drive(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
}

/// Windows: the path `canonicalize` will open, checked before it is opened.
///
/// Win32 rewrites a path before opening it: `.` and `..` are applied as text and trailing dots and spaces are
/// dropped (`C:\link.` opens `C:\link`). So the path is first put in that form (`std::path::absolute` is
/// `GetFullPathNameW`, which touches no disk), then made verbatim (`\\?\C:\…`), which is opened exactly as
/// written. The walk then sees the same names Windows will, and every link among them must stay on a local drive.
#[cfg(windows)]
fn checked_local(given: &Path) -> Result<PathBuf, String> {
    if !on_a_local_drive(given) {
        return Err(NOT_LOCAL.into());
    }
    let full = verbatim(std::path::absolute(given).map_err(|_| UNREADABLE.to_string())?);
    links_stay_local(&full, link_target)?;
    Ok(full)
}

/// `C:\x` as `\\?\C:\x`; anything else as it is.
#[cfg(windows)]
fn verbatim(path: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    match path.components().next() {
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_)) => {
            let mut s = std::ffi::OsString::from(r"\\?\");
            s.push(path.as_os_str());
            PathBuf::from(s)
        }
        _ => path,
    }
}

/// Is `path` a symlink or a junction (both are name-surrogate reparse points, which `is_symlink` reports), and
/// where does it point? Read without following it. `None`: not a link, or not there (nothing past it opens).
#[cfg(windows)]
fn link_target(path: &Path) -> Result<Option<PathBuf>, ()> {
    use std::io::ErrorKind;
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::read_link(path).map(Some).map_err(|_| ()),
        Ok(_) => Ok(None),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => Ok(None),
        Err(_) => Err(()),
    }
}

/// More links than this on one path is a loop (Windows gives up after 63).
#[cfg(windows)]
const MAX_LINKS: usize = 40;

#[cfg(windows)]
enum Step {
    /// A path's root: `C:\`, `\\?\C:\`, `\\server\share\`, ….
    Root(PathBuf),
    /// A link target like `\x`: the root of the link's own drive.
    DriveRoot,
    Up,
    Name(std::ffi::OsString),
}

#[cfg(windows)]
fn steps(path: &Path) -> Vec<Step> {
    use std::path::Component;
    let mut root = PathBuf::new();
    let mut out = Vec::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => root.push(c),
            Component::CurDir => {}
            Component::ParentDir => out.push(Step::Up),
            Component::Normal(name) => out.push(Step::Name(name.to_os_string())),
        }
    }
    if matches!(path.components().next(), Some(Component::Prefix(_))) {
        out.insert(0, Step::Root(root));
    } else if path.has_root() {
        out.insert(0, Step::DriveRoot);
    }
    out
}

/// Follows `path` one name at a time the way Windows will, reading each link's target (`read_link`) instead of
/// opening through it, and refuses as soon as a target leaves this computer's drives: no connection is made.
#[cfg(windows)]
fn links_stay_local(path: &Path, read_link: impl Fn(&Path) -> Result<Option<PathBuf>, ()>) -> Result<(), String> {
    use std::path::Component;
    let mut pending: Vec<Step> = steps(path).into_iter().rev().collect();
    let mut here = PathBuf::new();
    let mut links = 0;
    while let Some(step) = pending.pop() {
        match step {
            Step::Root(root) => {
                if !on_a_local_drive(&root) {
                    return Err(NOT_LOCAL.into());
                }
                here = verbatim(root);
            }
            Step::DriveRoot => {
                let mut root = PathBuf::new();
                if let Some(Component::Prefix(p)) = here.components().next() {
                    root.push(p.as_os_str());
                }
                root.push(r"\");
                here = root;
            }
            Step::Up => {
                here.pop();
            }
            Step::Name(name) => {
                here.push(&name);
                let Some(target) = read_link(&here).map_err(|()| UNREADABLE.to_string())? else {
                    continue;
                };
                links += 1;
                if links > MAX_LINKS {
                    return Err(UNREADABLE.into());
                }
                here.pop(); // a relative target starts in the link's folder
                pending.extend(steps(&target).into_iter().rev());
            }
        }
    }
    Ok(())
}

/// `canonicalize` on Windows returns the verbatim form (`\\?\C:\…`), which Explorer does not open; give it the
/// usual form instead. Other systems' paths are already fine.
fn for_display(dir: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = dir.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            let b = rest.as_bytes();
            if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
                return PathBuf::from(rest);
            }
        }
    }
    dir
}

/// The file manager command for `dir`, with the folder as its only argument (on macOS after `-R`).
fn file_manager(dir: &Path) -> Command {
    #[cfg(windows)]
    let mut cmd = {
        // From the Windows folder itself, not a search: an `explorer.exe` next to Pawl or on PATH is never used.
        let windows = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        Command::new(windows.join("explorer.exe"))
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        // Reveal (Finder selects it) rather than open: `open` on a bundle would launch it.
        let mut c = Command::new("/usr/bin/open");
        c.arg("-R");
        c
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut cmd = Command::new("xdg-open");
    // An absolute path never starts with `-` or `/`-switch, so none of them can read it as an option.
    #[cfg(windows)]
    {
        // Explorer splits its command line at commas (`/select,…`), and Rust quotes an argument only when it has
        // a space or tab, so `C:\a,b` would open the wrong place: always quote it. A Windows path can't contain `"`.
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(explorer_arg(dir));
    }
    #[cfg(not(windows))]
    cmd.arg(dir);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd
}

/// `dir` as Explorer's one quoted argument.
#[cfg(windows)]
fn explorer_arg(dir: &Path) -> String {
    format!("\"{}\"", dir.display())
}

/// Start the file manager without waiting for it. A thread reaps it when it exits (`open` and `xdg-open` hand the
/// folder over and quit at once), so no zombie process is left behind.
fn show(dir: &Path) -> Result<(), String> {
    let mut child = file_manager(dir).spawn().map_err(|_| NO_FILE_MANAGER.to_string())?;
    let _ = std::thread::Builder::new().name("pawl-open-folder".into()).spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pawl-open-folder-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn canonical(p: &Path) -> PathBuf {
        for_display(std::fs::canonicalize(p).unwrap())
    }

    #[test]
    fn relative_paths_are_refused() {
        for p in ["", "   ", ".", "..", "some/folder", "./x", "../x"] {
            assert_eq!(resolve(p).unwrap_err(), NOT_ABSOLUTE, "{p:?}");
        }
        #[cfg(windows)]
        for p in [r"C:folder", r"\Windows", "/Windows"] {
            assert_eq!(resolve(p).unwrap_err(), NOT_ABSOLUTE, "{p:?}");
        }
    }

    #[test]
    fn a_file_is_refused() {
        let d = scratch("file");
        let f = d.join("notes.txt");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(resolve(f.to_str().unwrap()).unwrap_err(), NOT_A_FOLDER);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_folder_is_refused() {
        let d = scratch("missing");
        let gone = d.join("not-here");
        assert_eq!(resolve(gone.to_str().unwrap()).unwrap_err(), MISSING);
        assert_eq!(resolve(gone.join("deeper").to_str().unwrap()).unwrap_err(), MISSING);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dot_dot_is_resolved() {
        let d = scratch("dots");
        let a = d.join("a");
        std::fs::create_dir_all(a.join("b")).unwrap();
        let with_dots = a.join("b").join("..").join(".").join("b").join("..");
        let got = resolve(with_dots.to_str().unwrap()).unwrap();
        assert_eq!(got, canonical(&a));
        assert!(got.is_absolute());
        assert!(!got.to_string_lossy().contains(".."), "{got:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn spaces_and_commas_stay_one_argument() {
        let d = scratch("args");
        let odd = d.join("My notes, 2026 (draft)");
        std::fs::create_dir_all(&odd).unwrap();
        let dir = resolve(odd.to_str().unwrap()).unwrap();
        let cmd = file_manager(&dir);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_os_string()).collect();
        #[cfg(target_os = "macos")]
        assert_eq!(args, vec![std::ffi::OsString::from("-R"), dir.as_os_str().to_os_string()], "reveal, never open");
        #[cfg(not(any(windows, target_os = "macos")))]
        assert_eq!(args, vec![dir.as_os_str().to_os_string()]);
        let program = PathBuf::from(cmd.get_program());
        #[cfg(windows)]
        {
            assert!(program.is_absolute(), "{program:?}");
            assert!(program.ends_with("explorer.exe"), "{program:?}");
            assert_eq!(args, vec![std::ffi::OsString::from(format!("\"{}\"", dir.display()))]);
            assert!(!dir.to_string_lossy().contains('"'));
            // A comma without a space is quoted too.
            assert_eq!(explorer_arg(Path::new(r"C:\a,b")), r#""C:\a,b""#);
        }
        #[cfg(target_os = "macos")]
        assert_eq!(program, PathBuf::from("/usr/bin/open"));
        #[cfg(not(any(windows, target_os = "macos")))]
        assert_eq!(program, PathBuf::from("xdg-open"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn app_bundles_are_refused_where_macos_would_run_them() {
        let d = scratch("bundles");
        for name in ["Foo.app", "Tool.APP", "x.bundle", "Kit.Framework", "a.plugin", "Pane.prefPane", "Driver.KEXT"] {
            let b = d.join(name);
            std::fs::create_dir_all(&b).unwrap();
            assert_eq!(resolve_with(b.to_str().unwrap(), true).unwrap_err(), AN_APP, "{name}");
            // Where bundles mean nothing it is just a folder.
            assert_eq!(resolve_with(b.to_str().unwrap(), false).unwrap(), canonical(&b), "{name}");
        }
        // Inside a bundle is a plain folder, and so are names that only look like one.
        for name in ["Foo.app/Contents", "Foo.apps", "app", ".app", "notes.app.d", "My.Project"] {
            let f = d.join(name);
            std::fs::create_dir_all(&f).unwrap();
            assert_eq!(resolve_with(f.to_str().unwrap(), true).unwrap(), canonical(&f), "{name}");
        }
        // The canonical name counts: a link named like a folder that points at an app is refused.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.join("Foo.app"), d.join("Notes")).unwrap();
            assert_eq!(resolve_with(d.join("Notes").to_str().unwrap(), true).unwrap_err(), AN_APP);
        }
        // Only macOS refuses them.
        let app = d.join("Foo.app");
        #[cfg(target_os = "macos")]
        assert_eq!(resolve(app.to_str().unwrap()).unwrap_err(), AN_APP);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(resolve(app.to_str().unwrap()).unwrap(), canonical(&app));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_are_local_and_in_the_usual_form() {
        // Refused before anything touches the network.
        for p in [r"\\server\share\folder", r"\\?\UNC\server\share", r"\\.\PhysicalDrive0", r"\\?\Volume{0}\"] {
            assert_eq!(resolve(p).unwrap_err(), NOT_LOCAL, "{p:?}");
        }
        assert_eq!(for_display(PathBuf::from(r"\\?\C:\Users\x")), PathBuf::from(r"C:\Users\x"));
        assert_eq!(for_display(PathBuf::from(r"C:\x")), PathBuf::from(r"C:\x"));
        // What `canonicalize` says for a mapped network drive is refused after the fact.
        assert!(!on_a_local_drive(Path::new(r"\\?\UNC\nas\share\x")));
        assert!(on_a_local_drive(Path::new(r"\\?\C:\x")));
        assert_eq!(verbatim(PathBuf::from(r"C:\x")), PathBuf::from(r"\\?\C:\x"));
        assert_eq!(verbatim(PathBuf::from(r"\\?\C:\x")), PathBuf::from(r"\\?\C:\x"));
        // A verbatim drive path is fine as input and comes out in the usual form.
        let d = scratch("verbatim");
        let long = std::fs::canonicalize(&d).unwrap();
        assert!(long.to_string_lossy().starts_with(r"\\?\"), "{long:?}");
        let got = resolve(long.to_str().unwrap()).unwrap();
        assert!(!got.to_string_lossy().starts_with(r"\\"), "{got:?}");
        assert_eq!(got, canonical(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Walks `path` over a pretend file system where `links` are the only links; also returns what it looked at.
    #[cfg(windows)]
    fn walk(path: &str, links: &[(&str, &str)]) -> (Result<(), String>, Vec<PathBuf>) {
        let asked = std::cell::RefCell::new(Vec::new());
        let result = links_stay_local(&verbatim(PathBuf::from(path)), |p| {
            let p = for_display(p.to_path_buf());
            asked.borrow_mut().push(p.clone());
            Ok(links.iter().find(|(link, _)| Path::new(link) == p).map(|(_, target)| PathBuf::from(target)))
        });
        (result, asked.into_inner())
    }

    #[cfg(windows)]
    #[test]
    fn every_link_on_the_way_must_stay_on_this_computer() {
        // A link to a share: refused, and nothing behind it is looked at.
        let (r, asked) = walk(r"C:\w\share\sub", &[(r"C:\w\share", r"\\host\share")]);
        assert_eq!(r.unwrap_err(), NOT_LOCAL);
        assert_eq!(asked, [PathBuf::from(r"C:\w"), PathBuf::from(r"C:\w\share")]);
        // Every other way a target can leave the local drives.
        for target in [r"\\?\UNC\host\share", r"\\?\GLOBALROOT\Device\Mup\host\share", r"\\.\pipe\x", r"\\?\Volume{0}\"] {
            assert_eq!(walk(r"C:\w\l", &[(r"C:\w\l", target)]).0.unwrap_err(), NOT_LOCAL, "{target}");
        }
        // At the end of a chain: absolute, relative with `..`, then relative to the drive's root.
        let chain = [(r"C:\w\a", r"D:\b"), (r"D:\b", r"..\c"), (r"D:\c", r"\d"), (r"D:\d", r"\\host\s")];
        let (r, asked) = walk(r"C:\w\a\x", &chain);
        assert_eq!(r.unwrap_err(), NOT_LOCAL);
        assert_eq!(asked.last(), Some(&PathBuf::from(r"D:\d")));
        // Chains that stay local are fine, and so are paths without links.
        assert_eq!(walk(r"C:\w\a\x", &chain[..3]).0, Ok(()));
        assert_eq!(walk(r"C:\w\x", &[]).0, Ok(()));
        // A loop ends.
        assert_eq!(walk(r"C:\w\a", &[(r"C:\w\a", r"C:\w\b"), (r"C:\w\b", "a")]).0.unwrap_err(), UNREADABLE);
        // A link that can't be read is not passed through.
        assert_eq!(links_stay_local(Path::new(r"\\?\C:\w"), |_| Err(())).unwrap_err(), UNREADABLE);
    }

    /// A junction (no privilege needed, unlike a symlink) at `link` pointing at the NT path `target`, set the way
    /// any program can: `FSCTL_SET_REPARSE_POINT` with a mount point. Windows takes a `\??\UNC\…` target this way
    /// although `mklink /J` refuses one.
    #[cfg(windows)]
    fn junction(link: &Path, target: &str) {
        use std::ffi::c_void;
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn CreateFileW(
                name: *const u16,
                access: u32,
                share: u32,
                security: *const c_void,
                disposition: u32,
                flags: u32,
                template: *mut c_void,
            ) -> *mut c_void;
            fn DeviceIoControl(
                handle: *mut c_void,
                code: u32,
                input: *const c_void,
                input_len: u32,
                output: *mut c_void,
                output_len: u32,
                returned: *mut u32,
                overlapped: *mut c_void,
            ) -> i32;
            fn CloseHandle(handle: *mut c_void) -> i32;
        }
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const OPEN_EXISTING: u32 = 3;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
        const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

        std::fs::create_dir(link).unwrap();
        // REPARSE_DATA_BUFFER with a MountPointReparseBuffer: the substitute name, then an empty print name.
        let name: Vec<u16> = target.encode_utf16().collect();
        let names = name.iter().chain([0u16, 0].iter());
        let mut buf = Vec::new();
        buf.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buf.extend(((8 + (name.len() + 2) * 2) as u16).to_le_bytes()); // ReparseDataLength
        buf.extend(0u16.to_le_bytes()); // Reserved
        buf.extend(0u16.to_le_bytes()); // SubstituteNameOffset
        buf.extend(((name.len() * 2) as u16).to_le_bytes()); // SubstituteNameLength
        buf.extend((((name.len() + 1) * 2) as u16).to_le_bytes()); // PrintNameOffset
        buf.extend(0u16.to_le_bytes()); // PrintNameLength
        for w in names {
            buf.extend(w.to_le_bytes());
        }
        let wide: Vec<u16> = link.as_os_str().encode_wide().chain([0]).collect();
        let flags = FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS;
        // SAFETY: a NUL-terminated name and a buffer of the stated length; the handle is closed before returning.
        unsafe {
            let h = CreateFileW(wide.as_ptr(), GENERIC_WRITE, 0, std::ptr::null(), OPEN_EXISTING, flags, std::ptr::null_mut());
            assert_ne!(h as isize, -1, "{}", std::io::Error::last_os_error());
            let mut returned = 0;
            let ok = DeviceIoControl(
                h,
                FSCTL_SET_REPARSE_POINT,
                buf.as_ptr().cast(),
                buf.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            );
            let err = std::io::Error::last_os_error();
            CloseHandle(h);
            assert_ne!(ok, 0, "{err}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_to_a_share_is_refused_before_anything_connects() {
        let d = scratch("junction");
        let real = d.join("real");
        std::fs::create_dir(&real).unwrap();
        let (share, hop, notes) = (d.join("share"), d.join("hop"), d.join("notes"));
        junction(&share, r"\??\UNC\pawl-test.invalid\share");
        junction(&hop, &format!(r"\??\{}", share.display()));
        junction(&notes, &format!(r"\??\{}", real.display()));
        assert_eq!(std::fs::read_link(&share).unwrap(), PathBuf::from(r"\\pawl-test.invalid\share"));

        let s = |p: &Path| p.to_str().unwrap().to_string();
        assert_eq!(resolve(&s(&share)).unwrap_err(), NOT_LOCAL);
        assert_eq!(resolve(&s(&share.join("sub"))).unwrap_err(), NOT_LOCAL, "a folder behind it");
        assert_eq!(resolve(&s(&hop)).unwrap_err(), NOT_LOCAL, "through a local junction first");
        // Windows drops trailing dots and spaces and applies `..` before opening, so these all name `share`.
        for odd in [format!("{}.", s(&share)), format!("{} ", s(&share)), s(&d.join("x").join("..").join("share"))] {
            assert_eq!(resolve(&odd).unwrap_err(), NOT_LOCAL, "{odd:?}");
        }
        // A junction to a folder on this computer is fine and opens that folder.
        assert_eq!(resolve(&s(&notes)).unwrap(), canonical(&real));
        // A symlink, which Windows would follow to the server, where this account may make one (admin or
        // Developer Mode); the pretend-file-system test above covers the rest.
        let sym = d.join("sym");
        match std::os::windows::fs::symlink_dir(r"\\pawl-test.invalid\share", &sym) {
            Ok(()) => {
                assert_eq!(resolve(&s(&sym)).unwrap_err(), NOT_LOCAL);
                std::fs::remove_dir(&sym).unwrap();
            }
            Err(e) => eprintln!("no symlink check on this machine: {e}"),
        }

        for j in [&share, &hop, &notes] {
            std::fs::remove_dir(j).unwrap(); // the junction itself, never its target
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
