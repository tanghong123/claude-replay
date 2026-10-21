//! Observe macOS exit independently of executable lookup and signal authority.

use anyhow::{Context, Result};
use std::io;

pub(super) fn info(pid: i32) -> io::Result<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe {
        // XNU's nonzero arg enables proc_find_zombref. Without it, a process
        // leaving the live lookup can look absent before exit has completed.
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            1,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if read <= 0 {
        return Err(io::Error::last_os_error());
    }
    if read != size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Incomplete native process info",
        ));
    }
    Ok(info)
}

fn exit_observation(info: io::Result<libc::proc_bsdinfo>) -> Result<bool> {
    match info {
        Ok(info) => Ok(info.pbi_status == 5), // SZOMB, after XNU finishes exit cleanup.
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(true),
        Err(error) => Err(error).context("Observe recovered native process exit"),
    }
}

pub(super) fn exited(pid: i32) -> Result<bool> {
    exit_observation(info(pid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};

    #[test]
    fn only_zombies_or_esrch_acknowledge_exit() {
        let mut live = info(std::process::id() as i32).unwrap();
        assert!(!exit_observation(Ok(live)).unwrap());
        live.pbi_status = 5;
        assert!(exit_observation(Ok(live)).unwrap());
        assert!(exit_observation(Err(io::Error::from_raw_os_error(libc::ESRCH))).unwrap());
        for code in [libc::EPERM, libc::EACCES, libc::EINVAL, libc::EIO] {
            assert!(exit_observation(Err(io::Error::from_raw_os_error(code))).is_err());
        }
    }

    #[test]
    fn unreaped_child_remains_observable_as_a_zombie() {
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("60");
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = crate::runtime::StartupChild(Some(command.spawn().unwrap()));
        let pid = child.0.as_ref().unwrap().id() as i32;
        assert!(!exited(pid).unwrap());
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = info(pid).expect("unreaped child must be visible, including as a zombie");
            if status.pbi_status == 5 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned child did not become a zombie"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(exited(pid).unwrap());
        child.0.as_mut().unwrap().wait().unwrap();
        child.0 = None;
        assert!(exited(pid).unwrap());
    }
}
