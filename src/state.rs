//! Project resolution and file paths.
//!
//! The registry is one SQLite file at the project root (`plans.db`), committed
//! to git. Isolation is structural: every path derives from the project root.
//! Tools never accept a repo argument, so one project can never read another
//! project's data.

use std::path::PathBuf;

use anyhow::Context;

#[derive(Clone, Debug)]
pub struct Project {
    pub root: PathBuf,
}

impl Project {
    /// `MIND_REPO` env override (for tests), then git toplevel, then cwd.
    pub fn resolve() -> anyhow::Result<Self> {
        if let Ok(root) = std::env::var("MIND_REPO") {
            return Ok(Self {
                root: PathBuf::from(root),
            });
        }

        let toplevel = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output();
        if let Ok(out) = toplevel
            && out.status.success()
        {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(Self {
                    root: PathBuf::from(path),
                });
            }
        }

        Ok(Self {
            root: std::env::current_dir()?,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("plans.db")
    }

    /// Legacy folder layout, read by `adopt` only.
    pub fn plans_dir(&self) -> PathBuf {
        self.root.join("plans")
    }

    /// Legacy generated artifacts, deleted by `adopt`.
    pub fn plans_md(&self) -> PathBuf {
        self.root.join("plans.md")
    }

    pub fn plans_yaml(&self) -> PathBuf {
        self.root.join("plans.yaml")
    }
}

/// Global lessons file. One lesson per line:
/// `- [tag] lesson text <!--id:N-->`
pub fn brain_path() -> PathBuf {
    if let Ok(path) = std::env::var("MIND_BRAIN") {
        return PathBuf::from(path);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/opencode/brain.md")
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

pub fn write_file(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}
