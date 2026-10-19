//! Turn process termination into an orderly native-runtime shutdown on the main thread.

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn request_shutdown(_: libc::c_int) {
    // Signal handlers cannot lock the runtime or perform process cleanup.
    REQUESTED.store(true, Ordering::Relaxed);
}

pub struct ShutdownSignals(Vec<(libc::c_int, libc::sigaction)>);

impl ShutdownSignals {
    pub fn install() -> Result<Self> {
        let mut installed = Self(Vec::new());
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            // Both structures are initialized before the OS reads them.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = request_shutdown as *const () as usize;
            action.sa_flags = libc::SA_RESETHAND;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            if unsafe { libc::sigaction(signal, &action, &mut previous) } != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("Install Monitor native shutdown signal handler");
            }
            installed.0.push((signal, previous));
        }
        Ok(installed)
    }

    pub fn wait(&self) -> Result<()> {
        while !REQUESTED.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Cleanup can wait on a lifecycle transition. A second terminal signal
        // must retain its normal ability to end the host, including another kind.
        for (signal, _) in &self.0 {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = libc::SIG_DFL;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            if unsafe { libc::sigaction(*signal, &action, std::ptr::null_mut()) } != 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!("Restore default Monitor signal {signal} before cleanup")
                });
            }
        }
        Ok(())
    }
}

impl Drop for ShutdownSignals {
    fn drop(&mut self) {
        for (signal, previous) in &self.0 {
            if unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) } != 0 {
                eprintln!(
                    "warning: restore Monitor signal {signal}: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn termination_is_observed_without_exiting_before_cleanup() {
        if std::env::var_os("MONITOR_SHUTDOWN_TEST_CHILD").is_none() {
            for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "shutdown::tests::termination_is_observed_without_exiting_before_cleanup",
                        "--nocapture",
                    ])
                    .env("MONITOR_SHUTDOWN_TEST_CHILD", signal.to_string())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while child.try_wait().unwrap().is_none() {
                    if std::time::Instant::now() >= deadline {
                        child.kill().unwrap();
                        child.wait().unwrap();
                        panic!("Monitor shutdown signal was not observed within five seconds");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                let output = child.wait_with_output().unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
            }
            return;
        }
        let signals = super::ShutdownSignals::install().unwrap();
        let signal = std::env::var("MONITOR_SHUTDOWN_TEST_CHILD")
            .unwrap()
            .parse()
            .unwrap();
        unsafe { libc::raise(signal) };
        signals.wait().unwrap();
        assert!(super::REQUESTED.load(std::sync::atomic::Ordering::Relaxed));
    }
}

#[cfg(test)]
mod repeat_tests {
    #[test]
    fn a_second_signal_can_end_slow_cleanup() {
        use std::io::Write;
        use std::os::unix::process::ExitStatusExt;
        if std::env::var_os("MONITOR_REPEAT_SIGNAL_CHILD").is_some() {
            let signals = super::ShutdownSignals::install().unwrap();
            unsafe { libc::raise(libc::SIGHUP) };
            signals.wait().unwrap();
            println!("cleanup-started");
            std::io::stdout().flush().unwrap();
            unsafe { libc::raise(libc::SIGINT) };
            panic!("second termination signal was swallowed");
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "shutdown::repeat_tests::a_second_signal_can_end_slow_cleanup",
                "--nocapture",
            ])
            .env("MONITOR_REPEAT_SIGNAL_CHILD", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("second signal did not end cleanup within five seconds");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.signal(), Some(libc::SIGINT));
        assert!(String::from_utf8_lossy(&output.stdout).contains("cleanup-started"));
    }
}
