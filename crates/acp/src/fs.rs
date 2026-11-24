//! Client-side `fs/read_text_file` and `fs/write_text_file`, restricted to the session cwd.

use std::io;
use std::path::{Component, Path, PathBuf};

/// Refuse to read files bigger than this into memory for an agent.
pub(crate) const MAX_READ_BYTES: u64 = 32 * 1024 * 1024;

/// Canonicalize a session root (the cwd), stripping Windows verbatim prefixes for display.
pub(crate) fn canonical_root(cwd: &Path) -> io::Result<PathBuf> {
    let root = std::fs::canonicalize(cwd)?;
    if !root.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, format!("{} is not a directory", cwd.display())));
    }
    Ok(root)
}

/// Remove the `\\?\` prefix Windows adds to canonical paths when it is safe
/// (plain drive paths), so agents see `C:\repo` rather than `\\?\C:\repo`.
pub(crate) fn display_path(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            let b = rest.as_bytes();
            if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
                return PathBuf::from(rest);
            }
        }
    }
    p.to_path_buf()
}

fn outside(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{} is outside the session working directory", path.display()),
    )
}

/// Resolve `path` (absolute, or relative to `root`) to a canonical path inside `root`.
///
/// `root` must already be canonical. Symlinks are resolved before the check, so a
/// link inside the root that points outside it is rejected. When `must_exist` is
/// false the deepest existing ancestor is canonicalized and the remaining
/// components (which may not contain `..`) are appended.
pub(crate) fn resolve_in_root(root: &Path, path: &Path, must_exist: bool) -> io::Result<PathBuf> {
    let joined = if path.is_absolute() { path.to_path_buf() } else { root.join(path) };
    match std::fs::canonicalize(&joined) {
        Ok(real) => {
            if real.starts_with(root) {
                Ok(real)
            } else {
                Err(outside(path))
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound && !must_exist => {
            let mut existing = joined.as_path();
            let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
            let base = loop {
                match std::fs::canonicalize(existing) {
                    Ok(real) => break real,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        let name = existing.file_name().ok_or_else(|| outside(path))?;
                        rest.push(name);
                        existing = existing.parent().ok_or_else(|| outside(path))?;
                    }
                    Err(e) => return Err(e),
                }
            };
            if !base.starts_with(root) {
                return Err(outside(path));
            }
            let mut out = base;
            for name in rest.iter().rev() {
                // `file_name()` never yields `..` or `.`, but be explicit about it.
                match Path::new(name).components().next() {
                    Some(Component::Normal(n)) => out.push(n),
                    _ => return Err(outside(path)),
                }
            }
            Ok(out)
        }
        Err(e) => Err(e),
    }
}

/// Slice `content` to `limit` lines starting at 1-based `line`.
pub(crate) fn slice_lines(content: &str, line: Option<u32>, limit: Option<u32>) -> String {
    if line.is_none() && limit.is_none() {
        return content.to_string();
    }
    let skip = line.map(|l| l.saturating_sub(1) as usize).unwrap_or(0);
    let take = limit.map(|l| l as usize).unwrap_or(usize::MAX);
    content.split_inclusive('\n').skip(skip).take(take).collect()
}

pub(crate) async fn read_text_file(root: &Path, path: &Path, line: Option<u32>, limit: Option<u32>) -> io::Result<String> {
    let real = resolve_in_root(root, path, true)?;
    let meta = tokio::fs::metadata(&real).await?;
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a file", path.display())));
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is too large ({} bytes, limit {MAX_READ_BYTES})", path.display(), meta.len()),
        ));
    }
    let bytes = tokio::fs::read(&real).await?;
    let content = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    Ok(slice_lines(&content, line, limit))
}

pub(crate) async fn write_text_file(root: &Path, path: &Path, content: &str) -> io::Result<()> {
    let real = resolve_in_root(root, path, false)?;
    if real == root {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot write to the session directory itself"));
    }
    if let Some(parent) = real.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&real, content.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("pet-acp-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn slices_lines() {
        let c = "a\nb\nc\nd";
        assert_eq!(slice_lines(c, None, None), c);
        assert_eq!(slice_lines(c, Some(2), Some(2)), "b\nc\n");
        assert_eq!(slice_lines(c, Some(4), None), "d");
        assert_eq!(slice_lines(c, None, Some(1)), "a\n");
        assert_eq!(slice_lines(c, Some(10), None), "");
    }

    #[test]
    fn rejects_paths_outside_root() {
        let base = TempDir::new();
        let root_dir = base.0.join("root");
        std::fs::create_dir_all(root_dir.join("sub")).unwrap();
        std::fs::write(base.0.join("secret.txt"), "x").unwrap();
        std::fs::write(root_dir.join("sub").join("ok.txt"), "y").unwrap();
        let root = canonical_root(&root_dir).unwrap();

        assert!(resolve_in_root(&root, &root_dir.join("sub").join("ok.txt"), true).is_ok());
        assert!(resolve_in_root(&root, Path::new("sub/ok.txt"), true).is_ok());
        let err = resolve_in_root(&root, &root_dir.join("..").join("secret.txt"), true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let err = resolve_in_root(&root, &base.0.join("secret.txt"), true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // Non-existing targets: allowed inside, rejected outside or via `..` escapes.
        let p = resolve_in_root(&root, &root_dir.join("new").join("deep").join("f.txt"), false).unwrap();
        assert!(p.starts_with(&root));
        assert!(resolve_in_root(&root, &root_dir.join("..").join("evil.txt"), false).is_err());
        assert!(resolve_in_root(&root, &root_dir.join("new").join("..").join("..").join("evil.txt"), false).is_err());
        assert_eq!(resolve_in_root(&root, &root_dir.join("missing.txt"), true).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn read_and_write_round_trip() {
        let base = TempDir::new();
        let root = canonical_root(&base.0).unwrap();
        let target = base.0.join("dir").join("note.txt");
        write_text_file(&root, &target, "one\ntwo\nthree\n").await.unwrap();
        assert_eq!(read_text_file(&root, &target, None, None).await.unwrap(), "one\ntwo\nthree\n");
        assert_eq!(read_text_file(&root, &target, Some(2), Some(1)).await.unwrap(), "two\n");
        assert!(write_text_file(&root, &base.0, "x").await.is_err());
    }

    #[test]
    fn strips_verbatim_prefix() {
        #[cfg(windows)]
        {
            assert_eq!(display_path(Path::new(r"\\?\C:\repo")), PathBuf::from(r"C:\repo"));
            assert_eq!(display_path(Path::new(r"\\?\UNC\srv\share")), PathBuf::from(r"\\?\UNC\srv\share"));
        }
        assert_eq!(display_path(Path::new("relative")), PathBuf::from("relative"));
    }
}
