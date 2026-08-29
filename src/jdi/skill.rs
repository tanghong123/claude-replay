//! `agent-jdi install-skill` (#26): materialize the bundled **jdi-handoff** skill without
//! the source repo — a Homebrew-installed binary carries the skill content itself
//! (`include_str!`) and reproduces the exact topology of
//! `integrations/install-jdi-handoff.sh`, which remains the in-repo installer and the
//! executable spec (`tests/install_jdi_handoff.sh` pins its behavior):
//!
//! - the canonical Skill at `<agents-dir>/jdi-handoff/SKILL.md` (default `~/.agents/skills`
//!   — the shared root Codex reads directly),
//! - a Claude symlink `<claude-dir>/skills/jdi-handoff/SKILL.md` → canonical,
//! - the Claude command `<claude-dir>/commands/jdi-handoff.md` (a managed copy).
//!
//! Safety mirrors the script: installer-owned directories must not be symlinks, managed
//! files are replaced (never written through a link), and a pre-existing regular-file
//! Claude Skill is preserved once as `.pre-shared-backup`. One CLI-only addition: a
//! locally-MODIFIED canonical Skill/command is backed up once (`.pre-install-backup`)
//! before the bundled content replaces it — nothing is ever clobbered silently, and a
//! plain `brew upgrade` re-install stays flag-free.
//!
//! `agent-jdi uninstall-skill` is the mirror, and takes the same care in reverse: it removes
//! only what this installer still OWNS — a managed file whose content is the bundled content,
//! and the Claude symlink only while it still points at our canonical Skill. Anything you
//! edited, replaced or re-pointed is left where it is and named in the output, because an
//! uninstaller that deletes a file you changed is the same failure as an installer that
//! overwrites one. The `.pre-shared-backup` Skill the install displaced is RESTORED — it was
//! yours before we were here — while `.pre-install-backup` copies of your own edits are left
//! for you to keep or delete.

use anyhow::{anyhow, bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

const SKILL_MD: &str = include_str!("../../integrations/shared/skills/jdi-handoff/SKILL.md");
const COMMAND_MD: &str = include_str!("../../integrations/claude/commands/jdi-handoff.md");

pub(crate) fn cmd_install_skill(
    agents_dir: Option<PathBuf>,
    claude_dir: Option<PathBuf>,
) -> Result<()> {
    let (agents_dir, claude_dir) = roots(agents_dir, claude_dir)?;
    install_at(&agents_dir, &claude_dir)
}

/// The two roots both commands work in, defaulted from `HOME` — one definition, so install
/// and uninstall can never disagree about where the Skill lives.
fn roots(agents_dir: Option<PathBuf>, claude_dir: Option<PathBuf>) -> Result<(PathBuf, PathBuf)> {
    let home = || {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set; pass both --agents-dir and --claude-dir"))
    };
    Ok((
        match agents_dir {
            Some(dir) => dir,
            None => home()?.join(".agents").join("skills"),
        },
        match claude_dir {
            Some(dir) => dir,
            None => home()?.join(".claude"),
        },
    ))
}

fn install_at(agents_dir: &Path, claude_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(agents_dir)
        .with_context(|| format!("create {}", agents_dir.display()))?;
    std::fs::create_dir_all(claude_dir)
        .with_context(|| format!("create {}", claude_dir.display()))?;
    // Canonicalize AFTER creating the roots (mirrors the script's `pwd -P`), so the
    // overlap check below compares real paths, not spellings.
    let agents_dir = std::fs::canonicalize(agents_dir)?;
    let claude_dir = std::fs::canonicalize(claude_dir)?;

    let canonical_dir = agents_dir.join("jdi-handoff");
    let claude_skills_dir = claude_dir.join("skills");
    let claude_skill_dir = claude_skills_dir.join("jdi-handoff");
    let claude_command_dir = claude_dir.join("commands");
    let canonical_skill = canonical_dir.join("SKILL.md");
    let claude_skill = claude_skill_dir.join("SKILL.md");
    let claude_command = claude_command_dir.join("jdi-handoff.md");

    if canonical_skill == claude_skill {
        bail!(
            "canonical and Claude Skill targets overlap: {}",
            canonical_skill.display()
        );
    }

    ensure_managed_dir(&canonical_dir, "canonical Skill directory")?;
    ensure_managed_dir(&claude_skills_dir, "Claude skills directory")?;
    ensure_managed_dir(&claude_skill_dir, "Claude jdi-handoff Skill directory")?;
    ensure_managed_dir(&claude_command_dir, "Claude commands directory")?;

    preserve_local_modification(&canonical_skill, SKILL_MD)?;
    install_managed_file(SKILL_MD, &canonical_skill)?;

    // The Claude Skill must END as a symlink to the canonical copy. Replace an existing
    // link (wherever it pointed); preserve a regular file once — its content predates the
    // shared installer and must not be lost.
    match std::fs::symlink_metadata(&claude_skill) {
        Ok(meta) if meta.file_type().is_symlink() => {
            std::fs::remove_file(&claude_skill)
                .with_context(|| format!("replace managed symlink {}", claude_skill.display()))?;
        }
        Ok(_) => {
            let backup = claude_skill_dir.join("SKILL.md.pre-shared-backup");
            if std::fs::symlink_metadata(&backup).is_ok() {
                bail!(
                    "cannot preserve existing Claude Skill; backup already exists: {}",
                    backup.display()
                );
            }
            std::fs::rename(&claude_skill, &backup)
                .with_context(|| format!("preserve {}", claude_skill.display()))?;
            println!("Preserved previous Claude Skill: {}", backup.display());
        }
        Err(_) => {}
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&canonical_skill, &claude_skill)
        .with_context(|| format!("link {}", claude_skill.display()))?;
    #[cfg(not(unix))]
    std::fs::write(&claude_skill, SKILL_MD)
        .with_context(|| format!("write {}", claude_skill.display()))?;

    preserve_local_modification(&claude_command, COMMAND_MD)?;
    install_managed_file(COMMAND_MD, &claude_command)?;

    println!("Installed shared Skill: {}", canonical_skill.display());
    println!(
        "Linked Claude Skill:   {} -> {}",
        claude_skill.display(),
        canonical_skill.display()
    );
    println!("Installed Claude command: {}", claude_command.display());
    println!("Open a new session, then use $jdi-handoff in Codex or /jdi-handoff in Claude Code.");
    Ok(())
}

pub(crate) fn cmd_uninstall_skill(
    agents_dir: Option<PathBuf>,
    claude_dir: Option<PathBuf>,
) -> Result<()> {
    let (agents_dir, claude_dir) = roots(agents_dir, claude_dir)?;
    uninstall_at(&agents_dir, &claude_dir)
}

/// Remove the installed topology, and ONLY the parts of it still owned by the installer.
///
/// Order is load-bearing: the Claude symlink goes before the canonical file it points at (so
/// nothing is briefly dangling), a preserved pre-shared Skill is restored into the gap the
/// symlink leaves, and the two `jdi-handoff` directories are removed only if they end up
/// EMPTY — never recursively, because whatever else is in them was not put there by us. The
/// shared roots (`~/.agents/skills`, `~/.claude/skills`, `~/.claude/commands`) always stay:
/// they belong to every skill, not to this one.
fn uninstall_at(agents_dir: &Path, claude_dir: &Path) -> Result<()> {
    let Ok(agents_dir) = std::fs::canonicalize(agents_dir) else {
        println!(
            "Nothing to remove: {} does not exist.",
            agents_dir.display()
        );
        return Ok(());
    };
    let claude_dir = std::fs::canonicalize(claude_dir).unwrap_or_else(|_| claude_dir.to_path_buf());

    let canonical_dir = agents_dir.join("jdi-handoff");
    let claude_skill_dir = claude_dir.join("skills").join("jdi-handoff");
    let canonical_skill = canonical_dir.join("SKILL.md");
    let claude_skill = claude_skill_dir.join("SKILL.md");
    let claude_command = claude_dir.join("commands").join("jdi-handoff.md");

    // The same guard the install uses: never act through a link that could redirect the
    // removal somewhere else entirely.
    for (dir, label) in [
        (&canonical_dir, "canonical Skill directory"),
        (&claude_skill_dir, "Claude jdi-handoff Skill directory"),
    ] {
        if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("{label} must not be a symbolic link: {}", dir.display());
        }
    }

    let mut removed = Vec::new();
    let mut kept = Vec::new();

    // 1. The Claude command — a managed copy, removable only while it still IS that copy.
    remove_managed_file(&claude_command, COMMAND_MD, &mut removed, &mut kept);

    // 2. The Claude Skill symlink, removable only while it still points at our canonical
    //    file. A link the user re-pointed is theirs now; a regular file there is theirs too.
    match std::fs::symlink_metadata(&claude_skill) {
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(&claude_skill) {
            Ok(target) if target == canonical_skill => {
                if std::fs::remove_file(&claude_skill).is_ok() {
                    removed.push(claude_skill.clone());
                }
            }
            Ok(target) => kept.push((
                claude_skill.clone(),
                format!("points at {} , not ours", target.display()),
            )),
            Err(_) => kept.push((claude_skill.clone(), "unreadable symlink".to_string())),
        },
        Ok(_) => kept.push((
            claude_skill.clone(),
            "not a symlink — replaced by hand since the install".to_string(),
        )),
        Err(_) => {}
    }

    // 3. Give back the Skill the install displaced. It predates us; the install only ever
    //    borrowed its place.
    let pre_shared = claude_skill_dir.join("SKILL.md.pre-shared-backup");
    if std::fs::symlink_metadata(&pre_shared).is_ok()
        && std::fs::symlink_metadata(&claude_skill).is_err()
    {
        match std::fs::rename(&pre_shared, &claude_skill) {
            Ok(()) => println!(
                "Restored your previous Claude Skill: {}",
                claude_skill.display()
            ),
            Err(e) => kept.push((pre_shared.clone(), format!("could not restore: {e}"))),
        }
    }

    // 4. The canonical Skill, last — the symlink above pointed at it.
    remove_managed_file(&canonical_skill, SKILL_MD, &mut removed, &mut kept);

    // 5. Directories we created, only while empty.
    for dir in [&canonical_dir, &claude_skill_dir] {
        if std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none())
            && std::fs::remove_dir(dir).is_ok()
        {
            removed.push(dir.clone());
        }
    }

    if removed.is_empty() && kept.is_empty() {
        println!("Nothing to remove — the jdi-handoff Skill is not installed here.");
        return Ok(());
    }
    for path in &removed {
        println!("Removed {}", path.display());
    }
    for (path, why) in &kept {
        println!("Kept   {} ({why})", path.display());
    }
    if !kept.is_empty() {
        println!(
            "Those are yours, not the installer's — delete them by hand if you want them gone."
        );
    }
    Ok(())
}

/// Remove `target` if it is still the managed file this installer wrote. A locally edited
/// copy — or anything that is not a plain file — is reported and left alone.
fn remove_managed_file(
    target: &Path,
    bundled: &str,
    removed: &mut Vec<PathBuf>,
    kept: &mut Vec<(PathBuf, String)>,
) {
    let Ok(meta) = std::fs::symlink_metadata(target) else {
        return;
    };
    if meta.file_type().is_symlink() {
        kept.push((
            target.to_path_buf(),
            "a symlink, not our managed copy".into(),
        ));
        return;
    }
    if !meta.is_file() {
        kept.push((target.to_path_buf(), "not a regular file".into()));
        return;
    }
    match std::fs::read_to_string(target) {
        Ok(current) if current == bundled => match std::fs::remove_file(target) {
            Ok(()) => removed.push(target.to_path_buf()),
            Err(e) => kept.push((target.to_path_buf(), format!("could not remove: {e}"))),
        },
        Ok(_) => kept.push((target.to_path_buf(), "locally modified".into())),
        Err(e) => kept.push((target.to_path_buf(), format!("unreadable: {e}"))),
    }
}

/// An installer-owned directory must be a real directory — never a symlink a hostile or
/// stale link could redirect the install through.
fn ensure_managed_dir(path: &Path, label: &str) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            bail!("{label} must not be a symbolic link: {}", path.display());
        }
        if !meta.is_dir() {
            bail!("{label} is not a directory: {}", path.display());
        }
    }
    std::fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    Ok(())
}

/// A managed file that exists as a REGULAR file with locally-modified content is preserved
/// once (`<name>.pre-install-backup`) before the bundled content replaces it. Modified
/// AGAIN after that backup was taken: refuse, so repeated local edits are never silently
/// clobbered (move the changes into your own skill, or delete the file to re-install).
fn preserve_local_modification(target: &Path, bundled: &str) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(target) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Ok(()); // links are replaced wholesale; dirs are rejected at write time
    }
    let current = std::fs::read_to_string(target).unwrap_or_default();
    if current == bundled {
        return Ok(());
    }
    let backup = target.with_file_name(format!(
        "{}.pre-install-backup",
        target
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ));
    if std::fs::symlink_metadata(&backup).is_ok() {
        bail!(
            "{} is locally modified and its one-time backup already exists: {}\n\
             move your changes into a skill of your own (or remove the file) and re-run.",
            target.display(),
            backup.display()
        );
    }
    std::fs::copy(target, &backup).with_context(|| format!("preserve {}", target.display()))?;
    println!("Preserved locally-modified file: {}", backup.display());
    Ok(())
}

/// Write `content` at `target` atomically: temp file in the target's directory, then a
/// rename. An existing SYMLINK is removed first — a managed file is replaced, never
/// written through — and a directory target is refused.
fn install_managed_file(content: &str, target: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if !meta.file_type().is_symlink() && meta.is_dir() {
            bail!("managed file target is a directory: {}", target.display());
        }
        if meta.file_type().is_symlink() {
            std::fs::remove_file(target)
                .with_context(|| format!("replace managed symlink {}", target.display()))?;
        }
    }
    let dir = target
        .parent()
        .ok_or_else(|| anyhow!("managed file has no parent: {}", target.display()))?;
    let tmp = dir.join(format!(".jdi-handoff-install.{}", std::process::id()));
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(content.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).ok();
    }
    std::fs::rename(&tmp, target).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        anyhow!("cannot replace managed file {}: {error}", target.display())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "jdi-skill-install-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        (root.join(".agents/skills"), root.join(".claude"), root)
    }

    #[test]
    fn fresh_install_creates_the_shared_topology_and_is_idempotent() {
        let (agents, claude, root) = fixture("fresh");
        install_at(&agents, &claude).unwrap();
        install_at(&agents, &claude).unwrap(); // idempotent

        let canonical = std::fs::canonicalize(&agents)
            .unwrap()
            .join("jdi-handoff/SKILL.md");
        assert_eq!(std::fs::read_to_string(&canonical).unwrap(), SKILL_MD);
        let link = std::fs::canonicalize(&claude)
            .unwrap()
            .join("skills/jdi-handoff/SKILL.md");
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), canonical);
        let command = std::fs::canonicalize(&claude)
            .unwrap()
            .join("commands/jdi-handoff.md");
        assert_eq!(std::fs::read_to_string(&command).unwrap(), COMMAND_MD);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn locally_modified_canonical_is_backed_up_once_then_refused() {
        let (agents, claude, root) = fixture("modified");
        install_at(&agents, &claude).unwrap();
        let canonical = std::fs::canonicalize(&agents)
            .unwrap()
            .join("jdi-handoff/SKILL.md");

        std::fs::write(&canonical, "my local edits\n").unwrap();
        install_at(&agents, &claude).unwrap(); // first re-install preserves + replaces
        let backup = canonical.with_file_name("SKILL.md.pre-install-backup");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "my local edits\n"
        );
        assert_eq!(std::fs::read_to_string(&canonical).unwrap(), SKILL_MD);

        std::fs::write(&canonical, "modified again\n").unwrap();
        let error = install_at(&agents, &claude).unwrap_err();
        assert!(error.to_string().contains("locally modified"), "{error:#}");
        assert_eq!(
            std::fs::read_to_string(&canonical).unwrap(),
            "modified again\n",
            "a refused install must not touch the modified file"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pre_shared_claude_skill_file_is_preserved_once() {
        let (agents, claude, root) = fixture("preshared");
        let skill_dir = claude.join("skills/jdi-handoff");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "pre-shared content\n").unwrap();

        install_at(&agents, &claude).unwrap();
        let dir = std::fs::canonicalize(&claude)
            .unwrap()
            .join("skills/jdi-handoff");
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md.pre-shared-backup")).unwrap(),
            "pre-shared content\n"
        );
        assert!(std::fs::symlink_metadata(dir.join("SKILL.md"))
            .unwrap()
            .file_type()
            .is_symlink());
        install_at(&agents, &claude).unwrap(); // reinstall keeps the backup untouched
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md.pre-shared-backup")).unwrap(),
            "pre-shared content\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_managed_dir_is_refused_before_anything_is_written() {
        let (agents, claude, root) = fixture("dirlink");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&agents).unwrap();
        std::os::unix::fs::symlink(&outside, agents.join("jdi-handoff")).unwrap();

        let error = install_at(&agents, &claude).unwrap_err();
        assert!(
            error.to_string().contains("must not be a symbolic link"),
            "{error:#}"
        );
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "nothing may be written through the link"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn command_symlink_is_replaced_never_written_through() {
        let (agents, claude, root) = fixture("cmdlink");
        let victim = root.join("victim.txt");
        std::fs::write(&victim, "do not overwrite\n").unwrap();
        let cmd_dir = claude.join("commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::os::unix::fs::symlink(&victim, cmd_dir.join("jdi-handoff.md")).unwrap();

        install_at(&agents, &claude).unwrap();
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "do not overwrite\n"
        );
        let command = std::fs::canonicalize(&claude)
            .unwrap()
            .join("commands/jdi-handoff.md");
        assert!(!std::fs::symlink_metadata(&command)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&command).unwrap(), COMMAND_MD);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The round trip: uninstall leaves the roots exactly as it found them, removes the
    /// jdi-handoff directories it created, and is idempotent.
    #[test]
    fn uninstall_removes_the_whole_installed_topology_and_is_idempotent() {
        let (agents, claude, root) = fixture("uninstall");
        install_at(&agents, &claude).unwrap();
        let agents_r = std::fs::canonicalize(&agents).unwrap();
        let claude_r = std::fs::canonicalize(&claude).unwrap();

        uninstall_at(&agents, &claude).unwrap();
        for gone in [
            agents_r.join("jdi-handoff/SKILL.md"),
            agents_r.join("jdi-handoff"),
            claude_r.join("skills/jdi-handoff/SKILL.md"),
            claude_r.join("skills/jdi-handoff"),
            claude_r.join("commands/jdi-handoff.md"),
        ] {
            assert!(
                std::fs::symlink_metadata(&gone).is_err(),
                "still there: {}",
                gone.display()
            );
        }
        // The SHARED roots belong to every skill, not to this one.
        assert!(agents_r.is_dir() && claude_r.join("skills").is_dir());
        assert!(claude_r.join("commands").is_dir());

        uninstall_at(&agents, &claude).unwrap(); // idempotent — nothing left to remove
        std::fs::remove_dir_all(root).unwrap();
    }

    /// An uninstaller that deletes a file you edited is the same failure as an installer that
    /// overwrites one. A locally-modified managed file stays, and so does its directory.
    #[test]
    fn uninstall_keeps_locally_modified_files() {
        let (agents, claude, root) = fixture("uninstall-modified");
        install_at(&agents, &claude).unwrap();
        let canonical = std::fs::canonicalize(&agents)
            .unwrap()
            .join("jdi-handoff/SKILL.md");
        let command = std::fs::canonicalize(&claude)
            .unwrap()
            .join("commands/jdi-handoff.md");
        std::fs::write(&canonical, "my local edits\n").unwrap();

        uninstall_at(&agents, &claude).unwrap();
        assert_eq!(
            std::fs::read_to_string(&canonical).unwrap(),
            "my local edits\n",
            "an edited Skill is the user's, not the installer's"
        );
        assert!(
            canonical.parent().unwrap().is_dir(),
            "its directory survives with it"
        );
        // …while the untouched command, still byte-for-byte ours, does go.
        assert!(std::fs::symlink_metadata(&command).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The install displaced a pre-existing Claude Skill into `.pre-shared-backup`; the
    /// uninstall gives it back. It was there before we were.
    #[cfg(unix)]
    #[test]
    fn uninstall_restores_a_displaced_pre_shared_skill() {
        let (agents, claude, root) = fixture("uninstall-preshared");
        let skill_dir = claude.join("skills/jdi-handoff");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "pre-shared content\n").unwrap();
        install_at(&agents, &claude).unwrap();

        uninstall_at(&agents, &claude).unwrap();
        let dir = std::fs::canonicalize(&claude)
            .unwrap()
            .join("skills/jdi-handoff");
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
            "pre-shared content\n",
            "restored, not deleted"
        );
        assert!(
            std::fs::symlink_metadata(dir.join("SKILL.md.pre-shared-backup")).is_err(),
            "and the backup is consumed by the restore"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A Claude Skill symlink the user re-pointed elsewhere is theirs — the uninstall must
    /// not follow it, remove it, or touch what it points at.
    #[cfg(unix)]
    #[test]
    fn uninstall_leaves_a_repointed_symlink_and_its_target_alone() {
        let (agents, claude, root) = fixture("uninstall-repointed");
        install_at(&agents, &claude).unwrap();
        let mine = root.join("my-skill.md");
        std::fs::write(&mine, "my own skill\n").unwrap();
        let link = std::fs::canonicalize(&claude)
            .unwrap()
            .join("skills/jdi-handoff/SKILL.md");
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&mine, &link).unwrap();

        uninstall_at(&agents, &claude).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "a re-pointed link is not ours to remove"
        );
        assert_eq!(std::fs::read_to_string(&mine).unwrap(), "my own skill\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Uninstalling where nothing was installed is a no-op, not an error — the same shape a
    /// second `brew uninstall` has.
    #[test]
    fn uninstall_on_a_clean_machine_is_a_no_op() {
        let (agents, claude, root) = fixture("uninstall-clean");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::create_dir_all(&claude).unwrap();
        uninstall_at(&agents, &claude).unwrap();
        // …and with no roots at all.
        std::fs::remove_dir_all(&agents).unwrap();
        uninstall_at(&agents, &claude).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn overlapping_canonical_and_claude_skill_targets_are_refused() {
        let (_, claude, root) = fixture("overlap");
        let error = install_at(&claude.join("skills"), &claude).unwrap_err();
        assert!(error.to_string().contains("overlap"), "{error:#}");
        assert!(
            !claude.join("skills/jdi-handoff/SKILL.md").exists(),
            "overlap rejection must happen before writing a Skill"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
