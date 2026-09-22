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
//!
//! One registry per repo is enforced with words, never by silent
//! picks. Each registry dir carries a `repo-id` marker naming the repo
//! it hosts (origin URL and checkout root). A marker mismatch, a
//! legacy `plans.db` beside a live one, or a second dir claiming the
//! same repo all refuse with an explanation.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use rusqlite::{Connection, OpenFlags};

use crate::db;

/// Registry dir marker: names the repo a state dir hosts.
const MARKER: &str = "repo-id";
/// Slug length cap; longer slugs truncate and take a hash suffix.
const MAX_SLUG: usize = 200;

/// The slug length cap, for tests.
pub fn max_slug_len() -> usize {
    MAX_SLUG
}

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
    /// A project for `root` with an explicit state home. Tests and
    /// embedders use this; `resolve` derives the state home from the
    /// environment.
    pub fn at(root: &Path, state_home: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            root: root.to_path_buf(),
            state_home: state_home.to_path_buf(),
            slug: slug_for_root(root)?,
        })
    }

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
            Err(_) => {
                let home = std::env::var("HOME").unwrap_or_default();
                anyhow::ensure!(
                    !home.is_empty(),
                    "HOME is unset; set it or point MIND_STATE_HOME at the mind state dir"
                );
                PathBuf::from(home)
                    .join(".config")
                    .join("opencode")
                    .join("mind")
            }
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

    /// Writer serialization file, always beside the database. Guarded
    /// with `flock` around every non-SQLite critical section
    /// (`prepare`, `adopt`).
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

    /// Create the state dir, migrate a legacy registry, refuse a fork,
    /// then open the database. Every read and write path goes through
    /// here, so exactly one registry per repo can exist. Returns
    /// whether a legacy registry moved in.
    pub fn open_registry(&self) -> anyhow::Result<Connection> {
        self.prepare()?;
        db::open(&self.db_path())
    }

    /// The `open_registry` checks without opening the database. Returns
    /// whether a legacy `plans.db` moved into the state dir.
    pub fn prepare(&self) -> anyhow::Result<bool> {
        let _lock = self.lock_registry()?;
        let migrated = self.migrate_locked()?;
        self.claim_marker()?;
        Ok(migrated)
    }

    /// Exclusive lock over the registry dir's non-SQLite critical
    /// sections. Blocks while another process migrates or adopts; the
    /// sections are tiny.
    pub fn lock_registry(&self) -> anyhow::Result<RegistryLock> {
        let dir = self.state_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.lock_path())
            .with_context(|| format!("open {}", self.lock_path().display()))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .with_context(|| format!("lock {}", self.lock_path().display()))?;
        Ok(RegistryLock { _file: file })
    }

    /// The migration and fork checks, under `lock_registry`. Returns
    /// whether a legacy registry moved in.
    pub(crate) fn migrate_locked(&self) -> anyhow::Result<bool> {
        let new_db = self.db_path();
        let legacy = self.legacy_db_path();
        if new_db.exists() && legacy.exists() {
            bail!(
                "two registries for this repo: the live one at {} and a \
                 stray plans.db at {}; every tool reads the state-dir \
                 file — check the stray, then move or delete it by hand",
                new_db.display(),
                legacy.display()
            );
        }
        if !new_db.exists() && legacy.exists() {
            migrate_legacy(&legacy, &new_db)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Write the `repo-id` marker on first use, and refuse when the
    /// marker names a different repo. When this dir is fresh, also
    /// refuse if another state dir already hosts this repo — the slug
    /// changed under it (origin added later, rule change) and writing
    /// here would fork the history.
    pub(crate) fn claim_marker(&self) -> anyhow::Result<()> {
        let identity = self.identity()?;
        let marker = self.state_dir().join(MARKER);
        if !marker.exists() {
            let fresh = !self.db_path().exists();
            if fresh && let Some(other) = find_registry_of(&self.state_home, &self.slug, &identity)
            {
                bail!(
                    "this repo already has a registry at {} (this checkout \
                     resolves to {} — a changed origin or a rule change); \
                     move or delete the old registry, or point MIND_REPO at \
                     the checkout it belongs to",
                    other.display(),
                    self.state_dir().display()
                );
            }
            write_file(&marker, &identity)?;
            return Ok(());
        }
        let existing = std::fs::read_to_string(&marker)
            .with_context(|| format!("read {}", marker.display()))?;
        if !identity_matches(&existing, &identity) {
            bail!(
                "registry {} hosts another repo ({}), not this one ({}); \
                 delete the wrong registry or fix the slug derivation",
                self.state_dir().display(),
                existing.trim(),
                identity.trim()
            );
        }
        Ok(())
    }

    /// One line naming the repo: the origin URL when it exists, else
    /// the checkout root. Both forms carry the other field as context
    /// so identity survives a later `git remote add`.
    fn identity(&self) -> anyhow::Result<String> {
        let origin = origin_url(&self.root);
        let canon = canonical_root(&self.root);
        match &origin {
            Some(url) => Ok(format!("origin: {url}\nroot: {}\n", canon.display())),
            None => Ok(format!("root: {}\n", canon.display())),
        }
    }
}

/// Holds `flock(LOCK_EX)` on the registry's `plans.lock` until dropped.
pub struct RegistryLock {
    _file: std::fs::File,
}

/// Repo slug for `root`: sanitized origin URL when origin exists, else
/// the sanitized root basename. Fails only when both derive nothing
/// (root `/` with no origin, for example).
pub fn slug_for_root(root: &Path) -> anyhow::Result<String> {
    let mut slug = match origin_url(root) {
        Some(url) if !url.is_empty() => slug_from_url(&url),
        _ => None,
    }
    .or_else(|| {
        let base = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        sanitize_segment(&base)
    })
    .with_context(|| {
        format!(
            "cannot derive a registry slug for {} (no origin remote, empty basename)",
            root.display()
        )
    })?;
    if slug.len() > MAX_SLUG {
        // Keep the head, hash the whole slug so truncation stays unique.
        let hash = &format!("{:016x}", fnv1a(slug.as_bytes()))[..8];
        slug = format!("{}-{}", &slug[..MAX_SLUG - 9], hash);
    }
    Ok(slug)
}

fn origin_url(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

fn canonical_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

fn identity_matches(existing: &str, identity: &str) -> bool {
    let field = |text: &str, name: &str| -> Option<String> {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{name}: ")))
            .map(str::to_string)
    };
    // Same origin, or the same checkout: one repo either way. A moved
    // checkout with a new origin is a judgment call the refusal words
    // cover; anything matching on neither field is a different repo.
    if let (Some(a), Some(b)) = (field(existing, "origin"), field(identity, "origin"))
        && a == b
    {
        return true;
    }
    if let (Some(a), Some(b)) = (field(existing, "root"), field(identity, "root"))
        && a == b
    {
        return true;
    }
    false
}

/// Another state dir under `state_home` whose marker names `identity`.
/// Only fresh registries ask — an established registry never scans.
fn find_registry_of(state_home: &Path, own_slug: &str, identity: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(state_home).ok()? {
        let Ok(entry) = entry else { continue };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() || entry.file_name().to_string_lossy() == own_slug {
            continue;
        }
        let marker = entry.path().join(MARKER);
        let Ok(text) = std::fs::read_to_string(&marker) else {
            continue;
        };
        if identity_matches(&text, identity) {
            return Some(entry.path());
        }
    }
    None
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

/// 64-bit FNV-1a. Uniqueness suffix for truncated slugs.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
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

/// Move the legacy registry into the state dir. Same-filesystem moves
/// rename atomically. Across filesystems the file copies through
/// SQLite (`VACUUM INTO`): checkpoint-aware, consistent snapshot, no
/// journal sidecars lost — a byte copy could tear under a concurrent
/// writer. The source goes away only after the copy is in place.
fn migrate_legacy(legacy: &Path, target: &Path) -> anyhow::Result<()> {
    match std::fs::rename(legacy, target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            let tmp = target.with_file_name("plans.db.migrating");
            let _ = std::fs::remove_file(&tmp);
            let src = Connection::open_with_flags(legacy, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| format!("open legacy {}", legacy.display()))?;
            src.execute("VACUUM INTO ?1", [tmp.to_string_lossy().as_ref()])
                .with_context(|| format!("copy {} -> {}", legacy.display(), tmp.display()))?;
            drop(src);
            std::fs::rename(&tmp, target)
                .with_context(|| format!("swap in {}", target.display()))?;
            std::fs::remove_file(legacy).with_context(|| format!("remove {}", legacy.display()))?;
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("move {}", legacy.display())),
    }
}
