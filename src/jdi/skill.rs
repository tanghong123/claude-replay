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

use anyhow::{anyhow, bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

const SKILL_MD: &str = include_str!("../../integrations/shared/skills/jdi-handoff/SKILL.md");
const COMMAND_MD: &str = include_str!("../../integrations/claude/commands/jdi-handoff.md");

pub(crate) fn cmd_install_skill(
    agents_dir: Option<PathBuf>,
    claude_dir: Option<PathBuf>,
) -> Result<()> {
    let home = || {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set; pass both --agents-dir and --claude-dir"))
    };
    let agents_dir = match agents_dir {
        Some(dir) => dir,
        None => home()?.join(".agents").join("skills"),
    };
    let claude_dir = match claude_dir {
        Some(dir) => dir,
        None => home()?.join(".claude"),
    };
    install_at(&agents_dir, &claude_dir)
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
