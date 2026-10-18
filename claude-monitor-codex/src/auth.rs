//! A private, durable credential for the Monitor-to-Codex hop, never browser pairing.

use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const TOKEN_NAME: &str = "codex-server.token";

// Deliberately has no Debug/Display implementation: credentials must not enter diagnostics.
pub(super) struct ServerToken(String);

impl ServerToken {
    pub fn path(receipts: &Path) -> Result<PathBuf> {
        crate::validate_state_path(receipts)?;
        Ok(receipts
            .parent()
            .context("Receipt database needs a state directory")?
            .join(TOKEN_NAME))
    }

    pub fn load_or_create(receipts: &Path) -> Result<Self> {
        Self::load_or_create_with_permissions(receipts, |file| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Creation modes are filtered by umask; restore owner read/write
                // through the new descriptor before writing any credential.
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            #[cfg(not(unix))]
            let _ = file;
            Ok(())
        })
    }

    fn load_or_create_with_permissions(
        receipts: &Path,
        set_permissions: impl FnOnce(&File) -> std::io::Result<()>,
    ) -> Result<Self> {
        let path = Self::path(receipts)?;
        let parent = path.parent().expect("token has a state directory");
        let directory = CredentialDirectory::open(parent)?;
        directory.restrict()?;
        match Self::read(&directory) {
            Ok(token) => {
                // A concurrent creator may have linked the file but not synced yet.
                directory.sync()?;
                return Ok(token);
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
        // Directory listings must not reveal even a prefix of the credential.
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
        let temporary = format!(".codex-server-token-{nonce}");
        let mut file = directory.open_file(&temporary, true)?;
        let publish = (|| -> Result<()> {
            // Permission failures must reach the same cleanup as write/sync failures.
            set_permissions(&file).context("Restrict temporary native credential")?;
            file.write_all(value.as_bytes())?;
            file.sync_all()?;
            match directory.publish(&temporary) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error).context("Publish native credential"),
            }
            directory.sync()
        })();
        finish_publication(
            publish,
            directory
                .remove(&temporary)
                .context("Remove temporary native credential"),
        )?;
        Self::read(&directory)
    }

    fn read(directory: &CredentialDirectory) -> Result<Self> {
        let path = directory.path.join(TOKEN_NAME);
        let file = directory.open_file(TOKEN_NAME, false)?;
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

/// Pin credential operations to the directory inspected, even if its pathname is replaced.
struct CredentialDirectory {
    file: File,
    path: PathBuf,
}

impl CredentialDirectory {
    fn open(path: &Path) -> Result<Self> {
        let mut directories = fs::DirBuilder::new();
        directories.recursive(true);
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            directories.mode(0o700);
            options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        directories
            .create(path)
            .with_context(|| format!("Create native credential directory {}", path.display()))?;
        let file = options
            .open(path)
            .with_context(|| format!("Open native credential directory {}", path.display()))?;
        Ok(Self {
            file,
            path: path.to_owned(),
        })
    }

    fn restrict(&self) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = self.file.metadata().with_context(|| {
                format!(
                    "Inspect native credential directory {}",
                    self.path.display()
                )
            })?;
            if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
                bail!(
                    "Native credential directory {} must be a real directory owned by this user",
                    self.path.display()
                );
            }
            // File::set_permissions uses this descriptor, not a second pathname lookup.
            self.file
                .set_permissions(fs::Permissions::from_mode(0o700))
                .with_context(|| {
                    format!(
                        "Restrict native credential directory {}",
                        self.path.display()
                    )
                })?;
        }
        Ok(())
    }

    /// On Unix, creation requests mode 0600, filtered by umask. The caller must restore
    /// exact permissions through the returned descriptor inside its cleanup
    /// boundary before writing credential bytes.
    fn open_file(&self, name: &str, create: bool) -> Result<File> {
        #[cfg(unix)]
        let result = {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(name).expect("generated credential basename");
            let flags = if create {
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
            } else {
                libc::O_RDONLY
            };
            // SAFETY: the directory descriptor and NUL-terminated basename remain live.
            let fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                // SAFETY: openat returned a new owned descriptor; File closes it exactly once.
                Ok(unsafe { File::from_raw_fd(fd) })
            }
        };
        #[cfg(not(unix))]
        let result = OpenOptions::new()
            .read(!create)
            .write(create)
            .create_new(create)
            .open(self.path.join(name));
        result.with_context(|| format!("Open native credential {}", self.path.join(name).display()))
    }

    fn publish(&self, temporary: &str) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let name = std::ffi::CString::new(temporary).expect("generated credential basename");
            let token_name = std::ffi::CString::new(TOKEN_NAME).expect("credential basename");
            // SAFETY: both names are valid basenames relative to the same live directory.
            if unsafe {
                libc::linkat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    self.file.as_raw_fd(),
                    token_name.as_ptr(),
                    0,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        fs::hard_link(self.path.join(temporary), self.path.join(TOKEN_NAME))
    }

    fn remove(&self, temporary: &str) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let name = std::ffi::CString::new(temporary).expect("generated credential basename");
            // SAFETY: unlinkat removes only this basename in the live directory, without following links.
            if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        fs::remove_file(self.path.join(temporary))
    }

    fn sync(&self) -> Result<()> {
        supported_directory_sync(self.file.sync_all()).with_context(|| {
            format!(
                "Persist native credential directory {}",
                self.path.display()
            )
        })
    }
}

// Always attempt cleanup, but retain publication as the primary error if both fail.
fn finish_publication(publish: Result<()>, cleanup: Result<()>) -> Result<()> {
    match (publish, cleanup) {
        (Err(publish), Err(cleanup)) => Err(publish).context(format!(
            "Temporary native credential cleanup also failed: {cleanup:#}"
        )),
        (publish, cleanup) => publish.and(cleanup),
    }
}

fn supported_directory_sync(result: std::io::Result<()>) -> std::io::Result<()> {
    match result {
        #[cfg(unix)]
        Err(error)
            if error
                .raw_os_error()
                .is_some_and(|code| code == libc::EINVAL || code == libc::ENOTSUP) =>
        {
            Ok(())
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn replaced_directory_path_cannot_redirect_permissions_or_credential_operations() {
        use std::os::unix::fs::PermissionsExt;
        if std::env::var("NATIVE_TOKEN_REPLACED_DIRECTORY_UMASK_TEST").as_deref() == Ok("child") {
            // A separate Cargo target directory may leave the shared scratch parent absent.
            fs::create_dir_all(std::env::temp_dir()).unwrap();
            // Only this test runs in the child; do not change the parent suite's umask.
            unsafe {
                libc::umask(0o277);
            }
        } else {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "auth::tests::replaced_directory_path_cannot_redirect_permissions_or_credential_operations",
                    "--nocapture",
                ])
                .env("NATIVE_TOKEN_REPLACED_DIRECTORY_UMASK_TEST", "child")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed;"),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let root =
            std::env::temp_dir().join(format!("native-auth-replaced-{}", std::process::id()));
        crate::remove_test_scratch(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.join("state");
        let moved = root.join("original");
        let unrelated = root.join("unrelated");
        fs::create_dir_all(&unrelated).unwrap();
        fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o2775)).unwrap();
        let unrelated_mode = fs::metadata(&unrelated).unwrap().permissions().mode() & 0o7777;
        let directory = CredentialDirectory::open(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o775)).unwrap();
        fs::rename(&state, &moved).unwrap();
        std::os::unix::fs::symlink(&unrelated, &state).unwrap();
        directory.restrict().unwrap();
        let temporary = ".codex-server-token-0123456789abcdef0123456789abcdef";
        let mut file = directory.open_file(temporary, true).unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
        file.write_all("a".repeat(64).as_bytes()).unwrap();
        file.sync_all().unwrap();
        directory.publish(temporary).unwrap();
        directory.sync().unwrap();
        directory.remove(temporary).unwrap();
        assert_eq!(
            ServerToken::read(&directory).unwrap().bearer(),
            ServerToken::fixture().bearer()
        );
        assert_eq!(
            fs::metadata(&moved).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        assert_eq!(
            fs::metadata(&unrelated).unwrap().permissions().mode() & 0o7777,
            unrelated_mode
        );
        assert_eq!(fs::read_dir(&unrelated).unwrap().count(), 0);
        assert!(moved.join(TOKEN_NAME).exists());
        assert!(!moved.join(temporary).exists());
        crate::remove_test_scratch(&root);
    }

    #[cfg(unix)]
    #[test]
    fn directory_sync_tolerates_only_unsupported_operations() {
        for code in [libc::EINVAL, libc::ENOTSUP] {
            assert!(supported_directory_sync(Err(std::io::Error::from_raw_os_error(code))).is_ok());
        }
        for code in [libc::EIO, libc::EACCES, libc::ENOSPC] {
            assert_eq!(
                supported_directory_sync(Err(std::io::Error::from_raw_os_error(code)))
                    .unwrap_err()
                    .raw_os_error(),
                Some(code)
            );
        }
        assert!(supported_directory_sync(Ok(())).is_ok());
    }

    #[test]
    fn publication_failure_remains_the_cause_when_cleanup_also_fails() {
        let publication = || Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into());
        let cleanup = || Err(anyhow::anyhow!("synthetic cleanup failure"));
        let error = finish_publication(publication(), cleanup()).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(format!("{error:#}").contains("synthetic cleanup failure"));
        assert!(format!("{error:#}").contains(&std::io::ErrorKind::PermissionDenied.to_string()));
        assert!(finish_publication(publication(), Ok(())).is_err());
        assert!(finish_publication(Ok(()), cleanup()).is_err());
        assert!(finish_publication(Ok(()), Ok(())).is_ok());
    }

    #[test]
    fn permission_failure_removes_the_empty_temporary_credential() {
        let root =
            std::env::temp_dir().join(format!("native-auth-permissions-{}", std::process::id()));
        crate::remove_test_scratch(&root);
        let receipts = root.join("receipts.sqlite3");
        let error = ServerToken::load_or_create_with_permissions(&receipts, |file| {
            assert_eq!(file.metadata()?.len(), 0, "No secret may be written yet");
            assert_eq!(fs::read_dir(&root)?.count(), 1, "Temporary file must exist");
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        })
        .err()
        .expect("permission failure must prevent publication");
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(format!("{error:#}").contains("Restrict temporary native credential"));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let token = ServerToken::load_or_create(&receipts).unwrap();
        assert_eq!(
            ServerToken::load_or_create(&receipts).unwrap().bearer(),
            token.bearer()
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        crate::remove_test_scratch(&root);
    }

    #[cfg(unix)]
    #[test]
    fn restrictive_umask_still_creates_a_reusable_private_credential() {
        use std::os::unix::fs::PermissionsExt;
        if std::env::var("NATIVE_TOKEN_UMASK_TEST").as_deref() != Ok("child") {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "auth::tests::restrictive_umask_still_creates_a_reusable_private_credential",
                    "--nocapture",
                ])
                .env("NATIVE_TOKEN_UMASK_TEST", "child")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed;"),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        // Prepare the shared scratch parent before removing its owner write bit at creation.
        fs::create_dir_all(std::env::temp_dir()).unwrap();
        // This process runs only this test, so the process-wide umask is isolated.
        unsafe {
            libc::umask(0o277);
        }
        let root = std::env::temp_dir().join(format!("native-auth-umask-{}", std::process::id()));
        let path = root.join("receipts.sqlite3");
        let token = ServerToken::load_or_create(&path).unwrap();
        assert_eq!(
            fs::metadata(ServerToken::path(&path).unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            ServerToken::load_or_create(&path).unwrap().bearer(),
            token.bearer()
        );
        crate::remove_test_scratch(&root);
    }

    #[cfg(unix)]
    #[test]
    fn restrictive_umask_tests_leave_a_missing_temp_parent_writable() {
        let root =
            std::env::temp_dir().join(format!("native-auth-temp-parent-{}", std::process::id()));
        crate::remove_test_scratch(&root);
        fs::create_dir_all(&root).unwrap();
        for name in [
            "replaced_directory_path_cannot_redirect_permissions_or_credential_operations",
            "restrictive_umask_still_creates_a_reusable_private_credential",
        ] {
            let temp = root.join(name);
            assert!(!temp.exists());
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &format!("auth::tests::{name}"), "--nocapture"])
                .env("TMPDIR", &temp)
                .env_remove("NATIVE_TOKEN_REPLACED_DIRECTORY_UMASK_TEST")
                .env_remove("NATIVE_TOKEN_UMASK_TEST")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed;"),
                "{name}: {stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            // A later test must still be able to create scratch files in the shared parent.
            fs::write(temp.join("later-test"), b"writable").unwrap();
        }
        crate::remove_test_scratch(&root);
    }

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
