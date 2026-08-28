//! One-time migration into the committed repo registry (`plans.db`).
//!
//! Sources: the legacy hidden per-user database plus the legacy
//! `plans/<name>/` folders (README.md, discussion.md) and the generated
//! `plans.md` / `plans.yaml` artifacts. Refuses to run twice. Folder and
//! artifact deletes happen only after every import succeeded inside one
//! transaction, so a failure never loses the source of a partial import.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use rusqlite::{Connection, OpenFlags};

use crate::db::{self, Patch};
use crate::state::Project;

/// Legacy hidden registry directory (old mind-mcp layout).
pub fn default_legacy_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("MIND_DATA") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/share/mind-mcp")
}

/// Hex-encoded canonical project root — the old per-project DB file name.
fn legacy_slug(root: &Path) -> anyhow::Result<String> {
    let canon = root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", root.display()))?;
    Ok(canon
        .to_string_lossy()
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Copy a SQLite file through `VACUUM INTO`: checkpoint-aware, atomic target.
fn copy_legacy_db(legacy_db: &Path, target: &Path) -> anyhow::Result<()> {
    let src = Connection::open_with_flags(legacy_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open legacy {}", legacy_db.display()))?;
    src.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])
        .with_context(|| format!("copy {} -> {}", legacy_db.display(), target.display()))?;
    Ok(())
}

/// Subdirectories of `dir`, sorted. Symlinks are excluded: adopt never
/// reaches outside the repo, and deleting a symlinked folder could fail
/// mid-migration.
fn legacy_dirs(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let is_link = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir && !is_link {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

pub fn run(project: &Project, legacy_dir: &Path) -> anyhow::Result<String> {
    let db_path = project.db_path();
    if db_path.exists() {
        // An empty registry can only come from an earlier read that opened
        // (and thereby created) the file. Real content means: runs once.
        let conn = db::open(&db_path)?;
        let plans: i64 = conn.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0))?;
        anyhow::ensure!(
            plans == 0,
            "{} already holds {} plan(s); adopt runs once",
            db_path.display(),
            plans
        );
        std::fs::remove_file(&db_path)
            .with_context(|| format!("remove empty {}", db_path.display()))?;
    }

    let legacy_db = legacy_dir.join(format!("{}.db", legacy_slug(&project.root)?));
    let folders_dir = project.plans_dir();
    let has_folders = folders_dir.is_dir() && !legacy_dirs(&folders_dir)?.is_empty();
    let has_artifacts = project.plans_md().exists() || project.plans_yaml().exists();
    if !legacy_db.exists() && (has_folders || has_artifacts) {
        bail!(
            "legacy database {} not found, but legacy plans/ folders or \
             plans.md/plans.yaml exist; refusing a partial migration \
             (point at the right directory with MIND_DATA)",
            legacy_db.display()
        );
    }

    if legacy_db.exists() {
        copy_legacy_db(&legacy_db, &db_path)?;
    }

    // Creates the file when no legacy DB exists; upgrades the copied one
    // (new plan fields, todos, notes; drops the dead progress column).
    let conn = db::open(&db_path)?;

    let mut todo_count = 0usize;
    let mut note_count = 0usize;
    let mut folders = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    let mut empty: Vec<String> = Vec::new();
    let mut imported_dirs: Vec<PathBuf> = Vec::new();

    // One transaction across every folder import: any failure rolls back all
    // plan content, and nothing has been deleted yet, so adopt re-runs.
    let tx = conn.unchecked_transaction()?;
    if folders_dir.is_dir() {
        for dir in legacy_dirs(&folders_dir)? {
            let name = dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if db::get(&conn, &name)?.is_none() {
                skipped.push(name);
                continue;
            }
            let (t, n, f) = import_folder(&tx, &name, &dir)?;
            if t == 0 && n == 0 && !f {
                // Nothing recognized in this folder — keep it and say so.
                empty.push(name);
                continue;
            }
            imported_dirs.push(dir);
            todo_count += t;
            note_count += n;
            folders += 1;
        }
    }
    tx.commit()?;

    // All imports committed; only now may sources be deleted.
    for dir in &imported_dirs {
        std::fs::remove_dir_all(dir).with_context(|| format!("remove {}", dir.display()))?;
    }

    let mut removed: Vec<String> = Vec::new();
    for artifact in [project.plans_md(), project.plans_yaml()] {
        if artifact.exists() {
            std::fs::remove_file(&artifact)
                .with_context(|| format!("remove {}", artifact.display()))?;
            removed.push(
                artifact
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    if folders_dir.is_dir() && std::fs::read_dir(&folders_dir)?.next().is_none() {
        std::fs::remove_dir(&folders_dir)?;
    }

    let mut summary = format!(
        "adopted {folders} folder(s): {todo_count} todo(s), {note_count} note(s); registry at {}",
        db_path.display()
    );
    if !removed.is_empty() {
        summary.push_str(&format!("; removed {}", removed.join(", ")));
    }
    if !skipped.is_empty() {
        summary.push_str(&format!(
            "; left in place (no registry row): {}",
            skipped.join(", ")
        ));
    }
    if !empty.is_empty() {
        summary.push_str(&format!("; nothing recognized in: {}", empty.join(", ")));
    }
    Ok(summary)
}

/// Pull README sections into plan fields and discussion entries into notes.
/// Returns (todos imported, notes imported, any field imported).
fn import_folder(
    conn: &Connection,
    name: &str,
    dir: &Path,
) -> anyhow::Result<(usize, usize, bool)> {
    let readme = std::fs::read_to_string(dir.join("README.md")).unwrap_or_default();

    let goal = cut_section(&readme, "Goal");
    let context = cut_section(&readme, "Context");
    let definition_of_done = cut_section(&readme, "Definition of done");
    let review_type = review_header(&readme);

    // Unknown sections (design sketches, open points, scope notes, ...)
    // carry real content; fold them into context so nothing is dropped.
    let extras = extra_sections(&readme);
    let context = match (context, extras.is_empty()) {
        (Some(c), true) => Some(c),
        (Some(c), false) => Some(format!("{c}\n\n{extras}")),
        (None, true) => None,
        (None, false) => Some(extras),
    };

    let fields = goal.is_some()
        || context.is_some()
        || definition_of_done.is_some()
        || review_type.is_some();
    if fields {
        db::update(
            conn,
            name,
            &Patch {
                title: None,
                branch: None,
                status: None,
                order: None,
                merge_commit: None,
                goal: goal.as_deref(),
                context: context.as_deref(),
                definition_of_done: definition_of_done.as_deref(),
                review_type: review_type.as_deref(),
            },
        )?;
    }

    let mut todos = 0usize;
    let mut notes = 0usize;
    if let Some(body) = cut_section(&readme, "Steps") {
        for item in step_items(&body) {
            db::todo_add(conn, name, &item)?;
            todos += 1;
        }
    }

    let discussion = std::fs::read_to_string(dir.join("discussion.md")).unwrap_or_default();
    for block in note_blocks(&discussion) {
        db::note_add(conn, name, &block)?;
        notes += 1;
    }
    Ok((todos, notes, fields))
}

/// Every `## ` section not mapped to a plan field, rendered as
/// `## Title\n\nbody` blocks joined by blank lines. Fence-aware.
pub fn extra_sections(doc: &str) -> String {
    const KNOWN: [&str; 4] = ["Goal", "Context", "Definition of done", "Steps"];
    let mut extras: Vec<String> = Vec::new();
    let mut title: Option<String> = None;
    let mut body = String::new();
    let mut fence = false;

    let mut flush = |title: &mut Option<String>, body: &mut String| {
        if let Some(t) = title.take()
            && !KNOWN.contains(&t.as_str())
        {
            let trimmed = body.trim().to_string();
            if !trimmed.is_empty() {
                extras.push(format!("## {t}\n\n{trimmed}"));
            }
        }
        body.clear();
    };

    for line in doc.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            if title.is_some() {
                body.push_str(line);
                body.push('\n');
            }
            continue;
        }
        if fence {
            if title.is_some() {
                body.push_str(line);
                body.push('\n');
            }
            continue;
        }
        if let Some(h) = line.strip_prefix("## ") {
            flush(&mut title, &mut body);
            title = Some(h.trim().to_string());
            continue;
        }
        if title.is_some() {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(&mut title, &mut body);
    extras.join("\n\n")
}

/// `**Review:** deep (two reviewers)` -> "deep", when it names a valid type.
/// Scanned in the header block only (before the first `## ` heading).
fn review_header(readme: &str) -> Option<String> {
    for line in readme.lines() {
        if line.trim_start().starts_with("## ") {
            break;
        }
        if let Some(word) = review_from_line(line) {
            return Some(word);
        }
    }
    None
}

/// `**Review:** deep (two reviewers)` -> "deep", when it names a valid type.
pub fn review_from_line(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("**Review:**")?;
    let word = rest.split_whitespace().next()?;
    let word = word
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_lowercase();
    db::REVIEW_TYPES.contains(&word.as_str()).then_some(word)
}

/// Body of one `## <heading>` section, trimmed; None when absent or empty.
/// First occurrence wins. Fenced code blocks pass through as content and
/// their `##`-looking lines never terminate the section.
pub fn cut_section(doc: &str, heading: &str) -> Option<String> {
    let marker = format!("## {heading}");
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    let mut fence = false;
    for line in doc.lines() {
        if inside && line.trim_start().starts_with("```") {
            fence = !fence;
            out.push(line);
            continue;
        }
        if fence {
            if inside {
                out.push(line);
            }
            continue;
        }
        if inside {
            // Any next heading ends the section — including a repeated
            // marker (first occurrence wins).
            if line.starts_with("## ") {
                break;
            }
            out.push(line);
            continue;
        }
        if line.trim() == marker {
            inside = true;
        }
    }
    let body = out.join("\n").trim().to_string();
    if body.is_empty() { None } else { Some(body) }
}

/// Split a Steps section into items. A new item starts at column 0 with
/// `- `, `* `, or `N. `; indented lines always continue the previous item.
/// Wrapped lines join the previous item with one space.
pub fn step_items(body: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented && let Some(rest) = item_start(trimmed) {
            items.push(rest.to_string());
        } else if let Some(last) = items.last_mut() {
            last.push(' ');
            last.push_str(trimmed);
        }
    }
    items
}

fn item_start(trimmed: &str) -> Option<&str> {
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return Some(rest);
    }
    if let Some(dot) = trimmed.find(". ") {
        let head = &trimmed[..dot];
        if !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()) {
            return Some(&trimmed[dot + 2..]);
        }
    }
    None
}

/// Split a discussion file into note blocks: one per `## ` heading (heading
/// kept as first line), everything before the first heading as one block.
/// The file title and the template boilerplate line never become notes.
/// Split a discussion file into note blocks: one per `## ` heading (heading
/// kept as first line), everything before the first heading as one block.
/// The file title and the template boilerplate line never become notes.
pub fn note_blocks(doc: &str) -> Vec<String> {
    const BOILERPLATE: &str = "Log decisions and open points here. Append as you work.";
    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut seen_heading = false;
    let mut flush = |current: &mut String| {
        let trimmed = current.trim().to_string();
        current.clear();
        if !trimmed.is_empty() {
            blocks.push(trimmed);
        }
    };

    for line in doc.lines() {
        // Title and template boilerplate only exist before the first heading.
        if !seen_heading && (line.starts_with("# ") || line.trim() == BOILERPLATE) {
            continue;
        }
        if let Some(heading) = line.strip_prefix("## ") {
            seen_heading = true;
            flush(&mut current);
            current.push_str(heading.trim());
            current.push('\n');
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    flush(&mut current);
    blocks
}
