//! SQLite storage for the plan registry: plans, their dependency graph,
//! per-plan todos, and per-plan notes. The database file lives in the repo
//! and is committed to git.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use rusqlite::{Connection, OptionalExtension, params};

pub const STATUSES: [&str; 4] = ["pending", "in_progress", "partial", "done"];
pub const TODO_STATUSES: [&str; 3] = ["pending", "in_progress", "done"];
pub const REVIEW_TYPES: [&str; 3] = ["deep", "quick", "none"];

#[derive(Clone, Debug, serde::Serialize)]
pub struct Plan {
    pub name: String,
    pub title: String,
    pub branch: String,
    pub status: String,
    pub sort_order: i64,
    pub merge_commit: String,
    pub goal: String,
    pub context: String,
    pub definition_of_done: String,
    pub review_type: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Todo {
    pub id: i64,
    pub plan: String,
    pub text: String,
    pub status: String,
    pub sort_order: i64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Note {
    pub id: i64,
    pub plan: String,
    pub text: String,
    pub created_at: String,
}

pub const PLAN_COLS: &str = "name, title, branch, status, sort_order, merge_commit, \
                             goal, context, definition_of_done, review_type";

const BASE_SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS plans(
   name TEXT PRIMARY KEY,
   title TEXT NOT NULL,
   branch TEXT NOT NULL DEFAULT '',
   status TEXT NOT NULL CHECK(status IN ('pending','in_progress','partial','done')),
   sort_order INTEGER NOT NULL DEFAULT 0,
   merge_commit TEXT NOT NULL DEFAULT '',
   goal TEXT NOT NULL DEFAULT '',
   context TEXT NOT NULL DEFAULT '',
   definition_of_done TEXT NOT NULL DEFAULT '',
   review_type TEXT NOT NULL DEFAULT 'deep' CHECK(review_type IN ('deep','quick','none')),
   created_at TEXT NOT NULL DEFAULT (datetime('now')),
   updated_at TEXT NOT NULL DEFAULT (datetime('now'))
 );
 CREATE TABLE IF NOT EXISTS plan_deps(
   plan TEXT NOT NULL REFERENCES plans(name) ON DELETE CASCADE,
   depends_on TEXT NOT NULL REFERENCES plans(name) ON DELETE CASCADE,
   PRIMARY KEY(plan, depends_on)
 );
 CREATE TABLE IF NOT EXISTS plan_todos(
   id INTEGER PRIMARY KEY AUTOINCREMENT,
   plan TEXT NOT NULL REFERENCES plans(name) ON DELETE CASCADE ON UPDATE CASCADE,
   text TEXT NOT NULL,
   status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','in_progress','done')),
   sort_order INTEGER NOT NULL DEFAULT 0,
   created_at TEXT NOT NULL DEFAULT (datetime('now')),
   updated_at TEXT NOT NULL DEFAULT (datetime('now'))
 );
 CREATE TABLE IF NOT EXISTS plan_notes(
   id INTEGER PRIMARY KEY AUTOINCREMENT,
   plan TEXT NOT NULL REFERENCES plans(name) ON DELETE CASCADE ON UPDATE CASCADE,
   text TEXT NOT NULL,
   created_at TEXT NOT NULL DEFAULT (datetime('now'))
 );";

pub fn open(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Two agents may mutate the same committed registry at once.
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(BASE_SCHEMA)?;
    upgrade(&conn)?;
    Ok(conn)
}

/// Bring a pre-rename database (progress column, no plan fields) up to the
/// current shape. Idempotent; no-op on fresh databases. Two processes may
/// race past the column check, so duplicate/no-such-column errors are
/// success.
fn upgrade(conn: &Connection) -> anyhow::Result<()> {
    let cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(plans)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    const ADD: [(&str, &str); 4] = [
        ("goal", "TEXT NOT NULL DEFAULT ''"),
        ("context", "TEXT NOT NULL DEFAULT ''"),
        ("definition_of_done", "TEXT NOT NULL DEFAULT ''"),
        (
            "review_type",
            "TEXT NOT NULL DEFAULT 'deep' CHECK(review_type IN ('deep','quick','none'))",
        ),
    ];
    for (col, decl) in ADD {
        if cols.iter().any(|c| c == col) {
            continue;
        }
        if let Err(e) = conn.execute(&format!("ALTER TABLE plans ADD COLUMN {col} {decl}"), [])
            && !e.to_string().contains("duplicate column name")
        {
            return Err(e.into());
        }
    }
    if cols.iter().any(|c| c == "progress")
        && let Err(e) = conn.execute("ALTER TABLE plans DROP COLUMN progress", [])
        && !e.to_string().contains("no such column")
    {
        return Err(e.into());
    }
    Ok(())
}

fn row_to_plan(row: &rusqlite::Row) -> rusqlite::Result<Plan> {
    Ok(Plan {
        name: row.get("name")?,
        title: row.get("title")?,
        branch: row.get("branch")?,
        status: row.get("status")?,
        sort_order: row.get("sort_order")?,
        merge_commit: row.get("merge_commit")?,
        goal: row.get("goal")?,
        context: row.get("context")?,
        definition_of_done: row.get("definition_of_done")?,
        review_type: row.get("review_type")?,
    })
}

pub fn list(conn: &Connection, status: Option<&str>) -> anyhow::Result<Vec<Plan>> {
    let sql = match status {
        Some(_) => {
            format!("SELECT {PLAN_COLS} FROM plans WHERE status = ?1 ORDER BY sort_order, name")
        }
        None => format!("SELECT {PLAN_COLS} FROM plans ORDER BY sort_order, name"),
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
            &format!("SELECT {PLAN_COLS} FROM plans WHERE name = ?1"),
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
    if !REVIEW_TYPES.contains(&p.review_type.as_str()) {
        bail!(
            "invalid review_type '{}' (allowed: {})",
            p.review_type,
            REVIEW_TYPES.join(", ")
        );
    }
    conn.execute(
        &format!(
            "INSERT INTO plans({PLAN_COLS})
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
        ),
        params![
            p.name,
            p.title,
            p.branch,
            p.status,
            p.sort_order,
            p.merge_commit,
            p.goal,
            p.context,
            p.definition_of_done,
            p.review_type,
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
    pub order: Option<i64>,
    pub merge_commit: Option<&'a str>,
    pub goal: Option<&'a str>,
    pub context: Option<&'a str>,
    pub definition_of_done: Option<&'a str>,
    pub review_type: Option<&'a str>,
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
    if let Some(r) = patch.review_type
        && !REVIEW_TYPES.contains(&r)
    {
        bail!(
            "invalid review_type '{r}' (allowed: {})",
            REVIEW_TYPES.join(", ")
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
    if let Some(v) = patch.merge_commit {
        push_str!("merge_commit", v)
    }
    if let Some(v) = patch.goal {
        push_str!("goal", v)
    }
    if let Some(v) = patch.context {
        push_str!("context", v)
    }
    if let Some(v) = patch.definition_of_done {
        push_str!("definition_of_done", v)
    }
    if let Some(v) = patch.review_type {
        push_str!("review_type", v)
    }
    if let Some(v) = patch.order {
        sets.push("sort_order = ?");
        values.push(Box::new(v));
    }

    // Positional binding: sets[i] pairs with values[i].
    let sql = format!("UPDATE plans SET {} WHERE name = ?", sets.join(", "));
    values.push(Box::new(name.to_string()));
    let changed = conn.execute(
        &sql,
        rusqlite::params_from_iter(values.iter().map(|v| v.as_ref())),
    )?;
    anyhow::ensure!(changed == 1, "plan '{name}' vanished during update");
    Ok(())
}

pub fn delete(conn: &Connection, name: &str) -> anyhow::Result<()> {
    if get(conn, name)?.is_none() {
        bail!("unknown plan '{name}'");
    }
    let changed = conn.execute("DELETE FROM plans WHERE name = ?1", params![name])?;
    anyhow::ensure!(changed == 1, "plan '{name}' vanished during delete");
    Ok(())
}

/// Rename a plan and rewrite every edge that references it, both as a
/// dependency and as a dependent. Todos and notes follow through ON UPDATE
/// CASCADE. The graph shape never changes, so no cycle check runs.
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

/// Run `f` inside one BEGIN IMMEDIATE transaction. Multi-statement
/// mutations must use this: two agents (separate MCP servers, or CLI vs
/// MCP) share the committed plans.db, and the process-local write lock
/// cannot serialize them.
pub fn with_immediate<T>(
    conn: &Connection,
    f: impl FnOnce(&Connection) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match f(conn) {
        Ok(v) => {
            conn.execute_batch("COMMIT")?;
            Ok(v)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Pending plans whose every dependency is `done`.
pub fn ready(conn: &Connection) -> anyhow::Result<Vec<Plan>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PLAN_COLS} FROM plans p
             WHERE p.status = 'pending'
               AND NOT EXISTS (
                 SELECT 1 FROM plan_deps d JOIN plans q ON q.name = d.depends_on
                 WHERE d.plan = p.name AND q.status <> 'done')
             ORDER BY p.sort_order, p.name"
    ))?;
    let rows = stmt.query_map([], row_to_plan)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ---------- todos ----------

pub fn todo_add(conn: &Connection, plan: &str, text: &str) -> anyhow::Result<i64> {
    if get(conn, plan)?.is_none() {
        bail!("unknown plan '{plan}'");
    }
    let order: i64 = conn.query_row(
        "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM plan_todos WHERE plan = ?1",
        params![plan],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO plan_todos(plan, text, sort_order) VALUES(?1, ?2, ?3)",
        params![plan, text, order],
    )?;
    Ok(conn.last_insert_rowid())
}

pub struct TodoPatch<'a> {
    pub text: Option<&'a str>,
    pub status: Option<&'a str>,
    pub order: Option<i64>,
}

pub fn todo_edit(conn: &Connection, id: i64, patch: &TodoPatch) -> anyhow::Result<()> {
    if !todo_exists(conn, id)? {
        bail!("unknown todo id {id}");
    }
    if let Some(status) = patch.status
        && !TODO_STATUSES.contains(&status)
    {
        bail!(
            "invalid todo status '{status}' (allowed: {})",
            TODO_STATUSES.join(", ")
        );
    }

    let mut sets: Vec<&str> = vec!["updated_at = datetime('now')"];
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(t) = patch.text {
        sets.push("text = ?");
        values.push(Box::new(t.to_string()));
    }
    if let Some(s) = patch.status {
        sets.push("status = ?");
        values.push(Box::new(s.to_string()));
    }
    if let Some(o) = patch.order {
        sets.push("sort_order = ?");
        values.push(Box::new(o));
    }
    let sql = format!("UPDATE plan_todos SET {} WHERE id = ?", sets.join(", "));
    values.push(Box::new(id));
    let changed = conn.execute(
        &sql,
        rusqlite::params_from_iter(values.iter().map(|v| v.as_ref())),
    )?;
    anyhow::ensure!(changed == 1, "todo {id} vanished during edit");
    Ok(())
}

pub fn todo_remove(conn: &Connection, id: i64) -> anyhow::Result<()> {
    if !todo_exists(conn, id)? {
        bail!("unknown todo id {id}");
    }
    let changed = conn.execute("DELETE FROM plan_todos WHERE id = ?1", params![id])?;
    anyhow::ensure!(changed == 1, "todo {id} vanished during remove");
    Ok(())
}

fn todo_exists(conn: &Connection, id: i64) -> anyhow::Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT id FROM plan_todos WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn row_to_todo(row: &rusqlite::Row) -> rusqlite::Result<Todo> {
    Ok(Todo {
        id: row.get("id")?,
        plan: row.get("plan")?,
        text: row.get("text")?,
        status: row.get("status")?,
        sort_order: row.get("sort_order")?,
    })
}

const TODO_COLS: &str = "id, plan, text, status, sort_order";

pub fn todos_of(conn: &Connection, plan: &str) -> anyhow::Result<Vec<Todo>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TODO_COLS} FROM plan_todos WHERE plan = ?1 ORDER BY sort_order, id"
    ))?;
    let rows = stmt.query_map(params![plan], row_to_todo)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ---------- notes ----------

pub fn note_add(conn: &Connection, plan: &str, text: &str) -> anyhow::Result<i64> {
    if get(conn, plan)?.is_none() {
        bail!("unknown plan '{plan}'");
    }
    conn.execute(
        "INSERT INTO plan_notes(plan, text) VALUES(?1, ?2)",
        params![plan, text],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn notes_of(conn: &Connection, plan: &str) -> anyhow::Result<Vec<Note>> {
    let mut stmt = conn
        .prepare("SELECT id, plan, text, created_at FROM plan_notes WHERE plan = ?1 ORDER BY id")?;
    let rows = stmt.query_map(params![plan], |row| {
        Ok(Note {
            id: row.get("id")?,
            plan: row.get("plan")?,
            text: row.get("text")?,
            created_at: row.get("created_at")?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
