//! SQLite storage for plans and their dependency graph.

use std::path::Path;

use anyhow::{Context, bail};
use rusqlite::{Connection, OptionalExtension, params};

pub const STATUSES: [&str; 4] = ["pending", "in_progress", "partial", "done"];

#[derive(Clone, Debug, serde::Serialize)]
pub struct Plan {
    pub name: String,
    pub title: String,
    pub branch: String,
    pub status: String,
    pub progress: String,
    pub sort_order: i64,
    pub merge_commit: String,
}

pub fn open(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS plans(
           name TEXT PRIMARY KEY,
           title TEXT NOT NULL,
           branch TEXT NOT NULL DEFAULT '',
           status TEXT NOT NULL CHECK(status IN ('pending','in_progress','partial','done')),
           progress TEXT NOT NULL DEFAULT '',
           sort_order INTEGER NOT NULL DEFAULT 0,
           merge_commit TEXT NOT NULL DEFAULT '',
           created_at TEXT NOT NULL DEFAULT (datetime('now')),
           updated_at TEXT NOT NULL DEFAULT (datetime('now'))
         );
         CREATE TABLE IF NOT EXISTS plan_deps(
           plan TEXT NOT NULL REFERENCES plans(name) ON DELETE CASCADE,
           depends_on TEXT NOT NULL REFERENCES plans(name) ON DELETE CASCADE,
           PRIMARY KEY(plan, depends_on)
         );",
    )?;
    Ok(conn)
}

fn row_to_plan(row: &rusqlite::Row) -> rusqlite::Result<Plan> {
    Ok(Plan {
        name: row.get("name")?,
        title: row.get("title")?,
        branch: row.get("branch")?,
        status: row.get("status")?,
        progress: row.get("progress")?,
        sort_order: row.get("sort_order")?,
        merge_commit: row.get("merge_commit")?,
    })
}

const COLS: &str = "name, title, branch, status, progress, sort_order, merge_commit";

pub fn list(conn: &Connection, status: Option<&str>) -> anyhow::Result<Vec<Plan>> {
    let sql = match status {
        Some(_) => format!("SELECT {COLS} FROM plans WHERE status = ?1 ORDER BY sort_order, name"),
        None => format!("SELECT {COLS} FROM plans ORDER BY sort_order, name"),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = match status {
        Some(s) => stmt.query_map(params![s], row_to_plan),
        None => stmt.query_map([], row_to_plan),
    }?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn get(conn: &Connection, name: &str) -> anyhow::Result<Option<Plan>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM plans WHERE name = ?1"),
            params![name],
            row_to_plan,
        )
        .optional()?)
}

/// Direct dependencies (what this plan waits for).
pub fn deps_of(conn: &Connection, name: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT depends_on FROM plan_deps WHERE plan = ?1
         ORDER BY (SELECT sort_order FROM plans WHERE name = depends_on)",
    )?;
    let rows = stmt.query_map(params![name], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Direct dependents (who waits for this plan).
pub fn dependents_of(conn: &Connection, name: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT plan FROM plan_deps WHERE depends_on = ?1 ORDER BY plan")?;
    let rows = stmt.query_map(params![name], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn all_edges(conn: &Connection) -> anyhow::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT plan, depends_on FROM plan_deps ORDER BY plan")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn next_order(conn: &Connection) -> anyhow::Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM plans",
        [],
        |r| r.get(0),
    )?)
}

#[allow(clippy::too_many_arguments)]
pub fn insert(conn: &Connection, p: &Plan) -> anyhow::Result<()> {
    if !crate::state::valid_name(&p.name) {
        bail!(
            "invalid name '{}': use lowercase letters, digits, hyphens",
            p.name
        );
    }
    if !STATUSES.contains(&p.status.as_str()) {
        bail!(
            "invalid status '{}' (allowed: {})",
            p.status,
            STATUSES.join(", ")
        );
    }
    conn.execute(
        "INSERT INTO plans(name, title, branch, status, progress, sort_order, merge_commit)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            p.name,
            p.title,
            p.branch,
            p.status,
            p.progress,
            p.sort_order,
            p.merge_commit
        ],
    )?;
    Ok(())
}

/// Update provided fields only. `depends_on` here means: replace the whole
/// list. `None` leaves it untouched.
pub struct Patch<'a> {
    pub title: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub status: Option<&'a str>,
    pub progress: Option<&'a str>,
    pub order: Option<i64>,
    pub merge_commit: Option<&'a str>,
}

pub fn update(conn: &Connection, name: &str, patch: &Patch) -> anyhow::Result<()> {
    if get(conn, name)?.is_none() {
        bail!("unknown plan '{name}'");
    }
    if let Some(status) = patch.status
        && !STATUSES.contains(&status)
    {
        bail!(
            "invalid status '{status}' (allowed: {})",
            STATUSES.join(", ")
        );
    }

    let mut sets: Vec<&str> = vec!["updated_at = datetime('now')"];
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    macro_rules! push_str {
        ($col:expr, $val:expr) => {{
            sets.push(concat!($col, " = ?"));
            values.push(Box::new($val.to_string()));
        }};
    }
    if let Some(v) = patch.title {
        push_str!("title", v)
    }
    if let Some(v) = patch.branch {
        push_str!("branch", v)
    }
    if let Some(v) = patch.status {
        push_str!("status", v)
    }
    if let Some(v) = patch.progress {
        push_str!("progress", v)
    }
    if let Some(v) = patch.merge_commit {
        push_str!("merge_commit", v)
    }
    if let Some(v) = patch.order {
        sets.push("sort_order = ?");
        values.push(Box::new(v));
    }

    // Positional binding: sets[i] pairs with values[i].
    let sql = format!("UPDATE plans SET {} WHERE name = ?", sets.join(", "));
    values.push(Box::new(name.to_string()));
    conn.execute(
        &sql,
        rusqlite::params_from_iter(values.iter().map(|v| v.as_ref())),
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, name: &str) -> anyhow::Result<()> {
    if get(conn, name)?.is_none() {
        bail!("unknown plan '{name}'");
    }
    conn.execute("DELETE FROM plans WHERE name = ?1", params![name])?;
    Ok(())
}

/// Rename a plan and rewrite every edge that references it, both as a
/// dependency and as a dependent. The graph shape never changes, so no
/// cycle check runs.
pub fn rename(conn: &Connection, old: &str, new: &str) -> anyhow::Result<()> {
    if get(conn, old)?.is_none() {
        bail!("unknown plan '{old}'");
    }
    if !crate::state::valid_name(new) {
        bail!("invalid name '{new}': use lowercase letters, digits, hyphens");
    }
    if new == old {
        bail!("new name equals the current name");
    }
    if exists(conn, new)? {
        bail!("plan '{new}' already exists");
    }

    // plan_deps references plans(name) without ON UPDATE CASCADE. Defer FK
    // checks to COMMIT so the edge rewrites and the row rename land as one
    // atomic step.
    conn.pragma_update(None, "defer_foreign_keys", "ON")?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE plan_deps SET depends_on = ?1 WHERE depends_on = ?2",
        params![new, old],
    )?;
    tx.execute(
        "UPDATE plan_deps SET plan = ?1 WHERE plan = ?2",
        params![new, old],
    )?;
    tx.execute(
        "UPDATE plans SET name = ?1, updated_at = datetime('now') WHERE name = ?2",
        params![new, old],
    )?;
    tx.commit()?;
    Ok(())
}

fn exists(conn: &Connection, name: &str) -> anyhow::Result<bool> {
    Ok(get(conn, name)?.is_some())
}

/// Replace the whole dependency list of `plan`. Validates targets and rejects
/// cycles (a cycle is any path plan -> dep -> ... -> plan).
pub fn set_deps(conn: &Connection, plan: &str, deps: &[String]) -> anyhow::Result<()> {
    if !deps.iter().all(|d| d != plan) {
        bail!("plan cannot depend on itself");
    }
    for d in deps {
        if !exists(conn, d)? {
            bail!("unknown dependency plan '{d}'");
        }
    }

    // Cycle check over the proposed graph.
    let mut edges: Vec<(String, String)> = all_edges(conn)?
        .into_iter()
        .filter(|(p, _)| p != plan)
        .collect();
    edges.extend(deps.iter().map(|d| (plan.to_string(), d.clone())));

    let mut stack: Vec<&str> = deps.iter().map(String::as_str).collect();
    let mut seen: Vec<String> = Vec::new();
    while let Some(node) = stack.pop() {
        if node == plan {
            bail!("dependency cycle detected through '{plan}'");
        }
        if seen.iter().any(|s| s == node) {
            continue;
        }
        seen.push(node.to_string());
        for (p, d) in &edges {
            if p == node {
                stack.push(d.as_str());
            }
        }
    }

    conn.execute("DELETE FROM plan_deps WHERE plan = ?1", params![plan])?;
    for d in deps {
        conn.execute(
            "INSERT INTO plan_deps(plan, depends_on) VALUES(?1, ?2)",
            params![plan, d],
        )?;
    }
    Ok(())
}

/// Pending plans whose every dependency is `done`.
pub fn ready(conn: &Connection) -> anyhow::Result<Vec<Plan>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM plans p
             WHERE p.status = 'pending'
               AND NOT EXISTS (
                 SELECT 1 FROM plan_deps d JOIN plans q ON q.name = d.depends_on
                 WHERE d.plan = p.name AND q.status <> 'done')
             ORDER BY p.sort_order, p.name"
    ))?;
    let rows = stmt.query_map([], row_to_plan)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
