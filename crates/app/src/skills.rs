//! Bundled agent skills shipped inside the `bakedit` binary.
//!
//! Installing `bakedit` on any machine automatically makes the `code-walkthrough` skill
//! available under `~/.agents/skills/code-walkthrough/SKILL.md` so any coding agent (`agy`,
//! Claude Code, Codex, Cursor) gets interactive `bakedit --no-focus --highlight ... --speak ...`
//! walkthroughs out of the box.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MANAGED_MARKER: &str = "<!-- bakedit-bundled-skill -->";

const BUNDLED_SKILLS: &[(&str, &str)] = &[(
    "code-walkthrough",
    include_str!("../../../assets/skills/code-walkthrough/SKILL.md"),
)];

/// Default skills directory (`~/.agents/skills`).
pub fn default_skills_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".agents").join("skills"))
}

/// Install bundled skills into `~/.agents/skills`. When `force` is false, existing files are
/// only updated if they carry the `bakedit-bundled-skill` marker (preserving user edits).
pub fn install_default(force: bool) -> io::Result<usize> {
    let Some(dir) = default_skills_dir() else {
        return Ok(0);
    };
    install_into(&dir, force)
}

/// Install bundled skills into `skills_dir`. Returns how many files were written.
pub fn install_into(skills_dir: &Path, force: bool) -> io::Result<usize> {
    let mut written = 0usize;
    for &(name, content) in BUNDLED_SKILLS {
        let dir = skills_dir.join(name);
        let path = dir.join("SKILL.md");
        let should_write = if force || !path.exists() {
            true
        } else {
            match fs::read_to_string(&path) {
                Ok(existing) => existing.contains(MANAGED_MARKER) && existing != content,
                Err(_) => false,
            }
        };
        if should_write {
            fs::create_dir_all(&dir)?;
            fs::write(&path, content)?;
            written += 1;
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_when_missing_updates_when_managed_and_preserves_custom_edits() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(install_into(dir.path(), false).unwrap(), 1);
        let skill = dir.path().join("code-walkthrough/SKILL.md");
        assert!(skill.is_file());
        // Already up to date -> no rewrite.
        assert_eq!(install_into(dir.path(), false).unwrap(), 0);
        // Managed older copy -> updated automatically.
        fs::write(&skill, format!("{MANAGED_MARKER}\nold")).unwrap();
        assert_eq!(install_into(dir.path(), false).unwrap(), 1);
        // Custom user copy without marker -> preserved unless forced.
        fs::write(&skill, "# custom skill").unwrap();
        assert_eq!(install_into(dir.path(), false).unwrap(), 0);
        assert_eq!(fs::read_to_string(&skill).unwrap(), "# custom skill");
        assert_eq!(install_into(dir.path(), true).unwrap(), 1);
        assert!(fs::read_to_string(&skill).unwrap().contains(MANAGED_MARKER));
    }
}
