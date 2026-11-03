//! Where device private keys live.
//!
//! The engine only needs [`SecretStore`]. The app uses the platform
//! keychain/keystore (`KeychainSecretStore`, feature `keychain`).
//! [`FileSecretStore`] keeps secrets in a file readable only by the current
//! user (mode 0600 on Unix); tests and the CLI use it, and the app only with an
//! explicit development override. Its weakness is documented in the threat
//! model: anyone who can read the user's files can read the key.

use std::fs;
use std::io;
use std::path::PathBuf;

/// A small key-value store for secrets (device private keys), by name.
///
/// Implementations must make `set` durable before returning and must never
/// log values.
pub trait SecretStore: Send {
    /// The secret stored under `name`, or `None` if there is none.
    fn get(&self, name: &str) -> io::Result<Option<String>>;
    /// Creates or replaces the secret under `name`.
    fn set(&self, name: &str, value: &str) -> io::Result<()>;
    /// Removes the secret; succeeds if it did not exist.
    fn delete(&self, name: &str) -> io::Result<()>;
    /// Short description for diagnostics ("file", "keychain", …).
    fn kind(&self) -> &'static str;
}

/// Secrets as files (`<name>.secret`, mode 0600 on Unix) in one folder.
/// Used by tests and the CLI, and by the app only with an explicit
/// development override when no OS keychain exists.
pub struct FileSecretStore {
    dir: PathBuf,
}

impl FileSecretStore {
    /// A store rooted at `dir` (created on the first write).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe}.secret"))
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, name: &str) -> io::Result<Option<String>> {
        match fs::read_to_string(self.path(name)) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn set(&self, name: &str, value: &str) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path(name);
        let tmp = path.with_extension("tmp");
        {
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            io::Write::write_all(&mut f, value.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        if let Ok(d) = fs::File::open(&self.dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }

    fn delete(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.path(name)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn kind(&self) -> &'static str {
        "file"
    }
}

#[cfg(feature = "keychain")]
pub use keychain::KeychainSecretStore;

/// Device keys in the platform's credential store:
///
/// | Platform | Store |
/// |---|---|
/// | macOS | login Keychain |
/// | iOS | Data Protection keychain (requires a code-signed app) |
/// | Windows | Credential Manager |
/// | Linux / BSD | Secret Service (GNOME Keyring, KWallet, KeePassXC, …) |
/// | Android | `SharedPreferences` encrypted with a non-exportable Android Keystore key |
///
/// Entries are named `service` / `<scope>:<name>`, where the scope is the
/// library's device id, so several libraries on one machine never share keys.
#[cfg(feature = "keychain")]
mod keychain {
    use super::SecretStore;
    use std::io;
    use std::sync::Arc;

    /// The OS credential store for one service name and library scope.
    pub struct KeychainSecretStore {
        store: Arc<keyring_core::CredentialStore>,
        service: String,
        scope: String,
        kind: &'static str,
    }

    fn io_err(e: keyring_core::Error) -> io::Error {
        io::Error::other(format!("OS keychain: {e}"))
    }

    #[allow(unreachable_code)]
    fn platform_store(
    ) -> Result<(Arc<keyring_core::CredentialStore>, &'static str), keyring_core::Error> {
        #[cfg(target_os = "macos")]
        {
            return Ok((
                apple_native_keyring_store::keychain::Store::new()?,
                "macOS Keychain",
            ));
        }
        #[cfg(target_os = "ios")]
        {
            return Ok((
                apple_native_keyring_store::protected::Store::new()?,
                "iOS Keychain",
            ));
        }
        #[cfg(target_os = "windows")]
        {
            return Ok((
                windows_native_keyring_store::Store::new()?,
                "Windows Credential Manager",
            ));
        }
        #[cfg(target_os = "android")]
        {
            return Ok((
                android_native_keyring_store::Store::new()?,
                "Android Keystore",
            ));
        }
        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "android"))
        ))]
        {
            return Ok((
                zbus_secret_service_keyring_store::Store::new()?,
                "Secret Service",
            ));
        }
        Err(keyring_core::Error::NotSupportedByStore(
            "no OS credential store on this platform".into(),
        ))
    }

    impl KeychainSecretStore {
        /// Opens the platform store and checks that it answers. Fails when
        /// there is none (for example a Linux session without a Secret
        /// Service provider) — callers must not silently fall back.
        pub fn open(service: &str, scope: &str) -> io::Result<Self> {
            let (store, kind) = platform_store().map_err(io_err)?;
            let s = Self {
                store,
                service: service.to_string(),
                scope: scope.to_string(),
                kind,
            };
            // probe: a read of a name that normally does not exist
            match s.entry("availability-probe")?.get_password() {
                Ok(_) | Err(keyring_core::Error::NoEntry) => Ok(s),
                Err(e) => Err(io_err(e)),
            }
        }

        fn entry(&self, name: &str) -> io::Result<keyring_core::Entry> {
            self.store
                .build(&self.service, &format!("{}:{name}", self.scope), None)
                .map_err(io_err)
        }
    }

    impl SecretStore for KeychainSecretStore {
        fn get(&self, name: &str) -> io::Result<Option<String>> {
            match self.entry(name)?.get_password() {
                Ok(v) => Ok(Some(v)),
                Err(keyring_core::Error::NoEntry) => Ok(None),
                Err(e) => Err(io_err(e)),
            }
        }

        fn set(&self, name: &str, value: &str) -> io::Result<()> {
            self.entry(name)?.set_password(value).map_err(io_err)?;
            // read back: a store that silently drops writes must not be trusted
            match self.get(name)? {
                Some(v) if v == value => Ok(()),
                _ => Err(io::Error::other("OS keychain did not keep the stored key")),
            }
        }

        fn delete(&self, name: &str) -> io::Result<()> {
            match self.entry(name)?.delete_credential() {
                Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
                Err(e) => Err(io_err(e)),
            }
        }

        fn kind(&self) -> &'static str {
            self.kind
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Uses the real OS keychain; run manually on a desktop session:
        /// `cargo test -p penroam-sync --features keychain -- --ignored keychain`
        #[test]
        #[ignore]
        fn keychain_round_trip() {
            let scope = format!("test-{}", uuid::Uuid::new_v4());
            let s = KeychainSecretStore::open("dev.penroam.app.test", &scope).unwrap();
            assert_eq!(s.get("k").unwrap(), None);
            s.set("k", "secret\nvalue").unwrap();
            assert_eq!(s.get("k").unwrap().as_deref(), Some("secret\nvalue"));
            s.set("k", "replaced").unwrap();
            assert_eq!(s.get("k").unwrap().as_deref(), Some("replaced"));
            s.delete("k").unwrap();
            assert_eq!(s.get("k").unwrap(), None);
            s.delete("k").unwrap();
            eprintln!("store: {}", s.kind());
        }
    }
}
