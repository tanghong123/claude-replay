//! Linux thread-group exit observation, separate from permission to send signals.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub(super) struct ProcessStat {
    pub group: u32,
    pub start: String,
    zombie: bool,
    threads: u32,
}

impl ProcessStat {
    pub fn exited(&self) -> bool {
        // The leader can be a zombie before its workers release their file
        // descriptors. stat's num_threads includes those remaining workers.
        self.zombie && self.threads == 1
    }
}

pub(super) fn read_stat(root: &Path) -> Result<ProcessStat> {
    let stat = fs::read_to_string(root.join("stat")).context("Read native process stat")?;
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .context("Invalid native process stat")?
        .1
        .split_whitespace()
        .collect();
    if fields.len() < 20 || fields[0].len() != 1 {
        bail!("Invalid native process stat fields");
    }
    let threads = fields[17].parse().context("Read native thread count")?;
    if threads == 0 {
        bail!("Invalid native thread count: zero");
    }
    Ok(ProcessStat {
        group: fields[2].parse().context("Read native process group")?,
        start: fields[19].to_owned(),
        zombie: fields[0] == "Z",
        threads,
    })
}

pub(super) fn exited(root: &Path) -> Result<bool> {
    match read_stat(root) {
        Ok(stat) => Ok(stat.exited()),
        Err(error) => match fs::metadata(root) {
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Ok(true),
            // Missing exe, unreadable stat or a malformed snapshot cannot prove
            // exit while the process directory still exists. Keep the receipt.
            _ => Err(error).with_context(|| format!("Observe native exit at {}", root.display())),
        },
    }
}

pub(super) fn identity(root: &Path, pid: u32, boot: &Path) -> Result<Value> {
    let uid = fs::metadata(root)?.uid();
    if uid != unsafe { libc::geteuid() } {
        bail!("Native process belongs to another user");
    }
    let stat = read_stat(root)?;
    if stat.exited() {
        bail!("Native process has exited");
    }
    let executable = match fs::read_link(root.join("exe")) {
        Ok(path) => {
            let mut identity = super::executable_identity(root.join("exe"))?;
            identity["path"] = json!(path);
            identity
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(error) => return Err(error).context("Read native executable path"),
    };
    Ok(
        json!({"pid":pid,"group":stat.group,"uid":uid,"start":stat.start,
        "boot":fs::read_to_string(boot)?.trim(),"executable":executable}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_stat_uses_kernel_field_numbers() {
        let (root, _runtime) = crate::runtime::tests::fixture();
        // A literal /proc/PID/stat prefix, independent of write_stat's indexes:
        // state is field 3, pgrp 5, num_threads 20 and starttime 22.
        fs::write(
            root.join("stat"),
            "42 (native (fixture)) Z 1 42 42 0 -1 4194304 7 0 0 0 11 12 0 0 20 0 3 0 12345\n",
        )
        .unwrap();
        let stat = read_stat(&root).unwrap();
        assert_eq!(stat.group, 42);
        assert_eq!(stat.start, "12345");
        assert_eq!(stat.threads, 3);
        assert!(!stat.exited());
        crate::remove_test_scratch(&root);
    }

    fn write_stat(root: &Path, state: &str, threads: u32) {
        let mut fields = vec!["0".to_owned(); 20];
        fields[0] = state.to_owned();
        fields[2] = "42".into();
        fields[17] = threads.to_string();
        fields[19] = "12345".into();
        fs::write(
            root.join("stat"),
            format!("42 (native (fixture)) {}", fields.join(" ")),
        )
        .unwrap();
    }

    #[test]
    fn a_zombie_leader_with_workers_is_not_an_exited_process() {
        let (root, _runtime) = crate::runtime::tests::fixture();
        // Deliberately no exe link: it is not an exit condition.
        write_stat(&root, "S", 1);
        assert!(!exited(&root).unwrap());
        write_stat(&root, "Z", 3);
        assert!(!exited(&root).unwrap());
        let stat = read_stat(&root).unwrap();
        assert_eq!(stat.group, 42);
        assert_eq!(stat.start, "12345");
        let boot = root.join("boot-id");
        fs::write(&boot, "synthetic-boot\n").unwrap();
        let current = identity(&root, 42, &boot).unwrap();
        assert!(current["executable"].is_null());
        let mut record = current.clone();
        record["executable"] = json!({"path":"/old/removed-executable"});
        assert!(super::super::same_process(&record, &current));
        write_stat(&root, "Z", 2);
        assert!(!exited(&root).unwrap());
        write_stat(&root, "Z", 1);
        assert!(exited(&root).unwrap());
        assert!(identity(&root, 42, &boot).is_err());
        crate::remove_test_scratch(&root);
        assert!(exited(&root).unwrap());
    }

    #[test]
    fn unreadable_or_invalid_stat_is_not_exit_evidence() {
        let (root, _runtime) = crate::runtime::tests::fixture();
        assert!(
            exited(&root).is_err(),
            "missing stat with a present process"
        );
        fs::create_dir(root.join("stat")).unwrap();
        assert!(exited(&root).is_err(), "unreadable stat");
        fs::remove_dir(root.join("stat")).unwrap();
        fs::write(root.join("stat"), "invalid").unwrap();
        assert!(exited(&root).is_err(), "malformed stat");
        write_stat(&root, "Z", 0);
        assert!(exited(&root).is_err(), "invalid thread count");
        crate::remove_test_scratch(&root);
    }
}
