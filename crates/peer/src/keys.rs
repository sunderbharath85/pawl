//! Long-term X25519 device key (`data_dir/device.key`, 64 hex chars).

use crate::b64;
use anyhow::{bail, Context};
use crypto_box::{PublicKey, SalsaBox, SecretKey};
use std::fmt;
use std::io::Write;
use std::path::Path;

/// This hub's long-term X25519 key pair. The secret never leaves this struct: there is no accessor,
/// and `Debug` prints only the public key.
pub struct DeviceKeys {
    secret: SecretKey,
    public: [u8; 32],
}

impl DeviceKeys {
    /// Load the key from `path`, or create it (CSPRNG, 64 hex chars, unix mode 0600) when the file
    /// does not exist. A file that exists but does not hold a valid key is an error, never silently
    /// replaced: the key is this hub's identity and every pairing depends on it.
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(mut raw) => {
                let keys = Self::from_hex_file(&raw);
                wipe(&mut raw);
                keys.with_context(|| {
                    format!(
                        "device key file {} is corrupt; delete it to create a new key (paired machines must then be paired again)",
                        path.display()
                    )
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::create(path),
            Err(e) => Err(e).with_context(|| format!("reading device key file {}", path.display())),
        }
    }

    /// A fresh random key pair (memory only).
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("OS random number generator unavailable");
        let keys = Self::from_secret_bytes(bytes);
        wipe(&mut bytes);
        keys
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    /// Base64 (standard, padded) public key, as it appears on the wire and in `Device.publicKey`.
    pub fn public_b64(&self) -> String {
        b64::encode(&self.public)
    }

    pub(crate) fn salsa_box(&self, peer_pk: &[u8; 32]) -> SalsaBox {
        SalsaBox::new(&PublicKey::from(*peer_pk), &self.secret)
    }

    pub(crate) fn from_secret_bytes(bytes: [u8; 32]) -> Self {
        let secret = SecretKey::from(bytes);
        let public = secret.public_key().to_bytes();
        DeviceKeys { secret, public }
    }

    fn from_hex_file(raw: &[u8]) -> anyhow::Result<Self> {
        let text = raw.trim_ascii();
        if text.len() != 64 {
            bail!("expected 64 hex characters");
        }
        let mut bytes = [0u8; 32];
        if hex::decode_to_slice(text, &mut bytes).is_err() {
            bail!("not hex");
        }
        let keys = Self::from_secret_bytes(bytes);
        wipe(&mut bytes);
        Ok(keys)
    }

    fn create(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let keys = Self::generate();
        let mut secret = keys.secret.to_bytes();
        let mut text = hex::encode(secret).into_bytes();
        wipe(&mut secret);
        text.push(b'\n');
        let mut opts = std::fs::OpenOptions::new();
        // create_new: two hubs racing on first start must not overwrite each other's key.
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let result = opts.open(path).and_then(|mut f| {
            f.write_all(&text)?;
            f.sync_all()
        });
        wipe(&mut text);
        match result {
            Ok(()) => Ok(keys),
            // Lost the race: use the key the other process wrote.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Self::load_or_create(path),
            Err(e) => Err(e).with_context(|| format!("writing device key file {}", path.display())),
        }
    }
}

impl fmt::Debug for DeviceKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceKeys").field("public", &self.public_b64()).finish_non_exhaustive()
    }
}

/// Overwrite key material before it is freed. Volatile writes so the compiler cannot drop them.
pub(crate) fn wipe(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive reference into `buf`.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut r = [0u8; 8];
        getrandom::fill(&mut r).unwrap();
        std::env::temp_dir().join(format!("krypto-peer-{tag}-{}", hex::encode(r)))
    }

    #[test]
    fn load_or_create_persists() {
        let dir = temp_dir("keys");
        let path = dir.join("nested").join("device.key");
        let a = DeviceKeys::load_or_create(&path).unwrap();
        let b = DeviceKeys::load_or_create(&path).unwrap();
        assert_eq!(a.public_key(), b.public_key());
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.trim().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corrupt_file_is_an_error_and_kept() {
        let dir = temp_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("device.key");
        std::fs::write(&path, "zz").unwrap();
        let err = DeviceKeys::load_or_create(&path).unwrap_err();
        assert!(format!("{err:#}").contains("corrupt"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "zz");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn debug_shows_only_public_key() {
        let k = DeviceKeys::generate();
        let dbg = format!("{k:?}");
        assert!(dbg.contains(&k.public_b64()));
        let secret_hex = hex::encode(k.secret.to_bytes());
        assert!(!dbg.contains(&secret_hex));
        assert_ne!(DeviceKeys::generate().public_key(), k.public_key());
    }
}
