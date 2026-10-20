//! Private restart receipts. A receipt never authorizes a signal without a fresh process check.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) struct Ownership {
    path: PathBuf,
    record: Value,
    // Keep one Monitor as owner even when several use the same state directory.
    _lease: File,
}

fn private_file(path: &Path, create: bool) -> Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let parent = path
        .parent()
        .context("Ownership receipt needs a directory")?;
    let directory = fs::symlink_metadata(parent)?;
    if !directory.is_dir()
        || directory.uid() != unsafe { libc::geteuid() }
        || directory.mode() & 0o077 != 0
    {
        bail!("Native ownership directory must be private and owned by this user");
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    if create {
        options.write(true).create_new(true).mode(0o600);
    }
    let file = options.open(path)?;
    if create {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        bail!("Native ownership file must be a single-link regular file owned by this user with mode 0600");
    }
    Ok(file)
}

impl Ownership {
    pub fn claim(state: &Path) -> Result<Self> {
        crate::auth::prepare_private_directory(
            state
                .parent()
                .context("Ownership needs a state directory")?,
        )?;
        let path = state.with_extension("owner.json");
        let lock = state.with_extension("owner.lock");
        let lease = match private_file(&lock, true) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) =>
            {
                private_file(&lock, false)?
            }
            Err(error) => return Err(error),
        };
        if unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&lease),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("Another Monitor holds native ownership");
        }
        Ok(Self {
            path,
            record: Value::Null,
            _lease: lease,
        })
    }

    pub fn record(&mut self, pid: u32, configuration: Value) -> Result<()> {
        let process = process_identity(pid)?;
        if process["group"].as_u64() != Some(u64::from(pid)) {
            bail!("Native server is not its own process-group leader");
        }
        self.record = json!({"version":1,"configuration":configuration,"process":process});
        let temporary = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        let mut file = private_file(&temporary, true)?;
        let result = (|| -> Result<()> {
            file.write_all(serde_json::to_string(&self.record)?.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)?;
            File::open(self.path.parent().unwrap())?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            if let Err(error) = fs::remove_file(&temporary) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("Remove native ownership temporary file: {error}");
                }
            }
        }
        result.context("Persist native server ownership before accepting activation")
    }

    /// Invoke only after the endpoint has passed the configured transport authentication.
    pub fn recover(mut self, configuration: &Value) -> Result<Self> {
        let file = private_file(&self.path, false)?;
        if file.metadata()?.len() > 16384 {
            bail!("Native ownership receipt is too large");
        }
        let mut bytes = Vec::new();
        file.take(16385).read_to_end(&mut bytes)?;
        self.record = serde_json::from_slice(&bytes).context("Read native ownership receipt")?;
        if self.record["version"] != 1
            || &self.record["configuration"] != configuration
            || self.record["process"]["group"] != self.record["process"]["pid"]
            || !self.matches()
        {
            bail!("Native ownership receipt does not match the configured running process");
        }
        Ok(self)
    }

    /// A temporarily missing listener is not proof that its recorded process exited.
    /// This is only a spawn fence; endpoint authentication is still required to adopt.
    pub fn has_live_record(&self, configuration: &Value) -> bool {
        let read = || -> Result<Value> {
            let mut bytes = Vec::new();
            private_file(&self.path, false)?
                .take(16385)
                .read_to_end(&mut bytes)?;
            Ok(serde_json::from_slice(&bytes)?)
        };
        read().ok().is_some_and(|record| {
            record["version"] == 1
                && &record["configuration"] == configuration
                && record["process"]["pid"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok())
                    .and_then(|pid| process_identity(pid).ok())
                    .is_some_and(|process| process == record["process"])
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.record["process"]["pid"].as_u64()?.try_into().ok()
    }

    pub fn matches(&self) -> bool {
        self.pid()
            .and_then(|pid| process_identity(pid).ok())
            .is_some_and(|current| current == self.record["process"])
    }

    pub fn clear(&self) -> Result<()> {
        // The lease serializes writers; preserve evidence if a different record appeared.
        let mut bytes = String::new();
        match private_file(&self.path, false) {
            Ok(file) => {
                file.take(16385).read_to_string(&mut bytes)?;
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(())
            }
            Err(error) => return Err(error),
        }
        if serde_json::from_str::<Value>(&bytes)? == self.record {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    /// Recovered servers are no longer children of this Monitor; never waitpid them.
    pub fn stop(&self) -> Result<()> {
        let pid = self.pid().context("Native ownership receipt has no pid")? as i32;
        for signal in [libc::SIGTERM, libc::SIGKILL] {
            if !self.matches() {
                bail!("Native process identity changed; refusing to signal it");
            }
            if unsafe { libc::kill(-pid, signal) } != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("Signal recovered native server group");
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if !self.matches() {
                    // The old leader exited. Never signal a potentially reused pid/group.
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        bail!("Recovered native server did not exit after TERM and KILL")
    }
}

fn executable_identity(path: PathBuf) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(&path)?;
    Ok(
        json!({"path":path,"device":metadata.dev(),"inode":metadata.ino(),"modified":metadata.mtime(),"nanos":metadata.mtime_nsec()}),
    )
}

#[cfg(target_os = "macos")]
fn process_identity(pid: u32) -> Result<Value> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if read != size || info.pbi_uid != unsafe { libc::geteuid() } || info.pbi_status == 5 {
        bail!("Native process is absent, exited or owned by another user");
    }
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let read =
        unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
    if read <= 0 {
        bail!("Cannot identify native process executable");
    }
    path.truncate(
        path.iter()
            .position(|byte| *byte == 0)
            .unwrap_or(read as usize),
    );
    use std::os::unix::ffi::OsStringExt;
    let executable = executable_identity(PathBuf::from(std::ffi::OsString::from_vec(path)))?;
    Ok(
        json!({"pid":pid,"group":info.pbi_pgid,"uid":info.pbi_uid,"start":[info.pbi_start_tvsec,info.pbi_start_tvusec],"executable":executable}),
    )
}

#[cfg(target_os = "linux")]
fn process_identity(pid: u32) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let root = PathBuf::from(format!("/proc/{pid}"));
    let uid = fs::metadata(&root)?.uid();
    if uid != unsafe { libc::geteuid() } {
        bail!("Native process belongs to another user");
    }
    let stat = fs::read_to_string(root.join("stat"))?;
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .context("Invalid process stat")?
        .1
        .split_whitespace()
        .collect();
    if fields.len() < 20 || fields[0] == "Z" {
        bail!("Native process has exited or has invalid stat");
    }
    let group: u32 = fields[2].parse()?;
    let mut executable = executable_identity(root.join("exe"))?;
    executable["path"] = json!(fs::read_link(root.join("exe"))?);
    Ok(
        json!({"pid":pid,"group":group,"uid":uid,"start":fields[19],"boot":fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim(),"executable":executable}),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_identity(_: u32) -> Result<Value> {
    bail!("Restart ownership is unsupported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};

    #[test]
    fn restart_receipts_reject_reused_identity_permissions_and_parallel_owners() {
        let (root, runtime) = super::super::tests::fixture();
        let state = root.join("receipts.sqlite3");
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("60");
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = super::super::StartupChild(Some(command.spawn().unwrap()));
        let pid = child.0.as_ref().unwrap().id();
        let configuration = runtime.identity();
        let mut receipt = Ownership::claim(&state).unwrap();
        receipt.record(pid, configuration.clone()).unwrap();
        let path = receipt.path.clone();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            Ownership::claim(&state).is_err(),
            "one owner holds the lease"
        );
        let bytes = fs::read(&path).unwrap();
        drop(receipt);
        let receipt = Ownership::claim(&state)
            .unwrap()
            .recover(&configuration)
            .unwrap();
        assert_eq!(receipt.pid(), Some(pid));
        assert!(receipt.matches());
        assert!(receipt.has_live_record(&configuration));
        drop(receipt);
        for mutate in ["pid", "group", "start", "executable"] {
            let mut record: Value = serde_json::from_slice(&bytes).unwrap();
            record["process"][mutate] = json!("different process identity");
            fs::write(&path, record.to_string()).unwrap();
            assert!(
                Ownership::claim(&state)
                    .unwrap()
                    .recover(&configuration)
                    .is_err(),
                "{mutate}"
            );
            assert!(child.0.as_mut().unwrap().try_wait().unwrap().is_none());
        }
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Ownership::claim(&state)
            .unwrap()
            .recover(&configuration)
            .is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Ownership::claim(&state)
            .unwrap()
            .recover(&json!({"endpoint":"different"}))
            .is_err());
        let receipt = Ownership::claim(&state)
            .unwrap()
            .recover(&configuration)
            .unwrap();
        receipt.stop().unwrap();
        child.0.as_mut().unwrap().wait().unwrap();
        child.0 = None;
        receipt.clear().unwrap();
        assert!(!path.exists());
        drop(receipt);
        drop(runtime);
        crate::remove_test_scratch(&root);
    }
}
