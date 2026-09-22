//! Project resolution and registry paths.
//!
//! The registry is one SQLite file per repo, stored outside every work
//! tree: `~/.config/opencode/mind/<repo-slug>/plans.db`. Git operations
//! in any checkout can never delete or fork it. The repo is identified
//! by its root (`MIND_REPO` pins it); resolution maps that root to the
//! state dir. The slug derives from the origin remote URL when one
//! exists — stable across clones and directory moves — and falls back
//! to the root basename for local-only repos. Isolation stays
//! structural: tools never accept a repo argument, so one project can
//! never read another project's data.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use rusqlite::Connection;

use crate::db;

#[derive(Clone, Debug)]
pub struct Project {
    /// Repo identity: `MIND_REPO` env override, then git toplevel, then cwd.
    pub root: PathBuf,
    /// Parent directory holding one registry dir per repo.
    /// `MIND_STATE_HOME` overrides (tests, isolation); default
    /// `$HOME/.config/opencode/mind`.
    pub state_home: PathBuf,
    /// Directory-safe repo slug: `owner-repo` from the origin URL when
    /// origin exists, else the root basename.
    pub slug: String,
}

impl Project {
    pub fn resolve() -> anyhow::Result<Self> {
        let root = if let Ok(root) = std::env::var("MIND_REPO") {
            PathBuf::from(root)
        } else if let Some(root) = git_toplevel() {
            root
        } else {
            std::env::current_dir()?
        };
        let state_home = match std::env::var("MIND_STATE_HOME") {
            Ok(dir) => PathBuf::from(dir),
            Err(_) => home_dir().join(".config").join("opencode").join("mind"),
        };
        let slug = slug_for_root(&root)?;
        Ok(Self {
            root,
            state_home,
            slug,
        })
    }

    /// The repo's registry dir: `<state_home>/<slug>/`.
    pub fn state_dir(&self) -> PathBuf {
        self.state_home.join(&self.slug)
    }

    pub fn db_path(&self) -> PathBuf {
        self.state_dir().join("plans.db")
    }

    /// Writer serialization file, always beside the database.
    pub fn lock_path(&self) -> PathBuf {
        self.state_dir().join("plans.lock")
    }

    /// Legacy location: the untracked plans.db at the repo root.
    fn legacy_db_path(&self) -> PathBuf {
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

    /// Create the state dir and lock file, migrate a legacy registry,
    /// refuse a fork, then open the database. Every read and write path
    /// goes through here, so exactly one registry per repo can exist.
    pub fn open_registry(&self) -> anyhow::Result<Connection> {
        self.prepare()?;
        db::open(&self.db_path())
    }

    /// The `open_registry` guarantees, without opening the database.
    /// Callers that need file-level access to the registry (adopt's
    /// `VACUUM INTO` copy) prepare first, then touch `db_path`.
    pub fn prepare(&self) -> anyhow::Result<()> {
        let dir = self.state_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let lock = self.lock_path();
        if !lock.exists() {
            std::fs::File::create(&lock).with_context(|| format!("create {}", lock.display()))?;
        }

        let new_db = self.db_path();
        let legacy = self.legacy_db_path();
        if new_db.exists() && legacy.exists() {
            bail!(
                "two registries for this repo: {} and {} both exist; \
                 move or delete the repo-root plans.db by hand, then retry",
                new_db.display(),
                legacy.display()
            );
        }
        if !new_db.exists() && legacy.exists() {
            migrate_legacy(&legacy, &new_db)?;
            eprintln!(
                "migrated plans.db {} -> {}",
                legacy.display(),
                new_db.display()
            );
        }
        Ok(())
    }
}

/// Repo slug for `root`: sanitized origin URL when origin exists, else
/// the sanitized root basename. Fails only when both derive nothing
/// (root `/` with no origin, for example).
pub fn slug_for_root(root: &Path) -> anyhow::Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["remote", "get-url", "origin"]).current_dir(root);
    if let Ok(out) = cmd.output()
        && out.status.success()
    {
        let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(slug) = slug_from_url(&url) {
            return Ok(slug);
        }
    }
    let base = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug = sanitize_segment(&base).with_context(|| {
        format!(
            "cannot derive a registry slug for {} (no origin remote, empty basename)",
            root.display()
        )
    })?;
    Ok(slug)
}

/// `owner-repo` slug from a git remote URL: `git@github.com:owner/repo.git`,
/// `https://user:pass@github.com/owner/repo.git`,
/// `ssh://git@host:2222/owner/repo.git` all give `owner-repo`. None when
/// the URL yields no usable segments.
pub fn slug_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let path: &str = match trimmed.split_once("://") {
        // URL form: the authority (host, port, credentials) precedes the
        // first slash; only the path identifies the repo.
        Some((_, rest)) => rest.split_once('/').map(|(_, p)| p).unwrap_or(""),
        None => match trimmed.split_once(':') {
            // scp-like `user@host:owner/repo`: the colon sits before any
            // slash, and the path follows it.
            Some((authority, p)) if !authority.contains('/') => p,
            // No scheme, no scp-like colon: treat the string as a path.
            _ => trimmed,
        },
    };
    let segments: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.strip_suffix(".git").unwrap_or(s).to_string())
        .collect();
    // Owner and repo: the last two path segments.
    let start = segments.len().saturating_sub(2);
    let parts: Vec<String> = segments[start..]
        .iter()
        .filter_map(|s| sanitize_segment(s))
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("-"))
}

/// Directory-safe, lowercase form of one slug segment.
fn sanitize_segment(raw: &str) -> Option<String> {
    let mut out = String::new();
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    // Collapse separator runs, trim leading/trailing separators.
    let mut collapsed = String::new();
    for c in out.chars() {
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }
    let trimmed = collapsed.trim_matches(|c| c == '-' || c == '.');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn git_toplevel() -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn home_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
}

/// Global lessons file. One lesson per line:
/// `- [tag] lesson text <!--id:N-->`
pub fn brain_path() -> PathBuf {
    if let Ok(path) = std::env::var("MIND_BRAIN") {
        return PathBuf::from(path);
    }
    home_dir().join(".config/opencode/brain.md")
}

/// Move the legacy registry into the state dir. Same-filesystem moves
/// rename atomically; across filesystems the file copies, flushes, and
/// swaps in, then the source goes away. The registry is a live shared
/// file: callers log the migration after it succeeds.
fn migrate_legacy(legacy: &Path, target: &Path) -> anyhow::Result<()> {
    match std::fs::rename(legacy, target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            let tmp = target.with_file_name("plans.db.migrating");
            std::fs::copy(legacy, &tmp)
                .with_context(|| format!("copy {} -> {}", legacy.display(), tmp.display()))?;
            let f =
                std::fs::File::open(&tmp).with_context(|| format!("flush {}", tmp.display()))?;
            f.sync_all()
                .with_context(|| format!("flush {}", tmp.display()))?;
            drop(f);
            std::fs::rename(&tmp, target)
                .with_context(|| format!("swap in {}", target.display()))?;
            std::fs::remove_file(legacy).with_context(|| format!("remove {}", legacy.display()))?;
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("move {}", legacy.display())),
    }
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
