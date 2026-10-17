//! A private, durable credential for the Monitor-to-Codex hop, never browser pairing.

use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

// Deliberately has no Debug/Display implementation: credentials must not enter diagnostics.
pub(super) struct ServerToken(String);

impl ServerToken {
    pub fn path(receipts: &Path) -> Result<PathBuf> {
        Ok(receipts
            .parent()
            .context("Receipt database needs a state directory")?
            .join("codex-server.token"))
    }

    pub fn load_or_create(receipts: &Path) -> Result<Self> {
        let path = Self::path(receipts)?;
        let parent = path.parent().expect("token has a state directory");
        let mut directories = fs::DirBuilder::new();
        directories.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directories.mode(0o700);
        }
        directories
            .create(parent)
            .with_context(|| format!("Create native credential directory {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = fs::symlink_metadata(parent).with_context(|| {
                format!("Inspect native credential directory {}", parent.display())
            })?;
            if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
                bail!(
                    "Native credential directory {} must be a real directory owned by this user",
                    parent.display()
                );
            }
            // Existing owner-controlled state directories may have inherited umask 002.
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).with_context(|| {
                format!("Restrict native credential directory {}", parent.display())
            })?;
        }
        match Self::read(&path) {
            Ok(token) => {
                // Another creator may have linked this file but not synced the directory yet.
                sync_directory(parent)?;
                return Ok(token);
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
        // The filename nonce is independent of the secret; directory listings must
        // not reveal even a prefix of the credential.
        let mut random = [0_u8; 48];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let value: String = random[..32]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let nonce: String = random[32..]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        // Publish only a fully written token. Concurrent Monitors reuse the winner.
        let temporary = parent.join(format!(".codex-server-token-{nonce}"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .context("Create native credential")?;
        let publish = (|| -> Result<()> {
            file.write_all(value.as_bytes())?;
            file.sync_all()?;
            match fs::hard_link(&temporary, &path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error).context("Publish native credential"),
            };
            // Sync even when another creator won: it may not have synced its link yet.
            sync_directory(parent)
        })();
        fs::remove_file(&temporary).context("Remove temporary native credential")?;
        publish?;
        Self::read(&path)
    }

    fn read(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .with_context(|| format!("Open native credential {}", path.display()))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != 64 {
            bail!(
                "Native credential {} must be a regular file containing 64 hexadecimal bytes",
                path.display()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o600 {
                bail!(
                    "Native credential {} must be owned by this user with mode 0600",
                    path.display()
                );
            }
        }
        let mut value = String::new();
        file.take(65).read_to_string(&mut value)?;
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("Native credential {} has an invalid format", path.display());
        }
        Ok(Self(value))
    }

    pub fn bearer(&self) -> tungstenite::http::HeaderValue {
        let mut value = tungstenite::http::HeaderValue::from_str(&format!("Bearer {}", self.0))
            .expect("validated hexadecimal credential");
        value.set_sensitive(true);
        value
    }

    #[cfg(test)]
    pub fn fixture() -> Self {
        Self("a".repeat(64))
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("Persist native credential directory {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn existing_owner_directory_is_tightened_but_symlink_directory_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("native-auth-directory-{}", std::process::id()));
        crate::remove_test_scratch(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o775)).unwrap();
        let receipts = root.join("receipts.sqlite3");
        let token = ServerToken::load_or_create(&receipts).unwrap();
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            ServerToken::load_or_create(&receipts).unwrap().bearer(),
            token.bearer()
        );

        let target = root.join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let error = ServerToken::load_or_create(&link.join("receipts.sqlite3"))
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains(link.to_str().unwrap()));
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let path = ServerToken::path(&receipts).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let error = ServerToken::load_or_create(&receipts).err().unwrap();
        let message = format!("{error:#}");
        assert!(message.contains(path.to_str().unwrap()));
        assert!(!message.contains(&token.0));
        assert!(!fs::read_dir(&root).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".codex-server-token-")));
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn credential_is_private_stable_and_concurrent_creation_is_atomic() {
        let root = std::env::temp_dir().join(format!("native-auth-{}", std::process::id()));
        crate::remove_test_scratch(&root);
        let path = root.join("receipts.sqlite3");
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || ServerToken::load_or_create(&path).unwrap().bearer())
            })
            .collect();
        let values: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert!(values.iter().all(|value| value == values[0]));
        assert!(values[0].is_sensitive());
        assert_eq!(
            ServerToken::load_or_create(&path).unwrap().bearer(),
            values[0]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let token_path = ServerToken::path(&path).unwrap();
            assert_eq!(
                fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(ServerToken::load_or_create(&path).is_err());
            fs::remove_file(&token_path).unwrap();
            std::os::unix::fs::symlink(root.join("missing"), &token_path).unwrap();
            assert!(ServerToken::load_or_create(&path).is_err());
        }
        crate::remove_test_scratch(&root);
    }
}
