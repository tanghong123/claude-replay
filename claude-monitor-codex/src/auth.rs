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
        fs::create_dir_all(parent).context("Create native credential directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = fs::symlink_metadata(parent)?;
            if !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o022 != 0
            {
                bail!("Native credential directory must be owned by this user and not writable by others");
            }
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        match Self::read(&path) {
            Ok(token) => return Ok(token),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
        let mut random = [0_u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let value: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        // Publish only a fully written token. Concurrent Monitors reuse the winner.
        let temporary = parent.join(format!(".codex-server-token-{}", &value[..16]));
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
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(error).context("Publish native credential"),
            }
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
        let file = options.open(path).context("Open native credential")?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != 64 {
            bail!("Native credential must be a regular file containing 64 hexadecimal bytes");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o600 {
                bail!("Native credential must be owned by this user with mode 0600");
            }
        }
        let mut value = String::new();
        file.take(65).read_to_string(&mut value)?;
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("Native credential has an invalid format");
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

#[cfg(test)]
mod tests {
    use super::*;

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
