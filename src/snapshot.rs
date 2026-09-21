//! Portable registry snapshots: deterministic YAML for export and import.
//!
//! The live registry is one machine-local SQLite file. A snapshot is the
//! portability layer: byte-stable for the same state, safe to commit, and
//! restorable on another machine. Export orders every list, so repeated
//! exports of an unchanged registry diff cleanly in git.

use anyhow::{Context, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::{self, Plan};

pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub plans: Vec<PlanEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlanEntry {
    pub name: String,
    pub title: String,
    pub branch: String,
    pub status: String,
    pub order: i64,
    pub merge_commit: String,
    pub goal: String,
    pub context: String,
    #[serde(rename = "definition_of_done")]
    pub definition_of_done: String,
    pub review_type: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub todos: Vec<TodoEntry>,
    #[serde(default)]
    pub notes: Vec<NoteEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TodoEntry {
    pub id: i64,
    pub text: String,
    pub status: String,
    pub order: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NoteEntry {
    pub id: i64,
    pub text: String,
    pub created_at: String,
}

/// Read the whole registry, every list in stable order.
pub fn export(conn: &Connection) -> anyhow::Result<Snapshot> {
    let plans: Vec<PlanEntry> = db::list(conn, None)?
        .into_iter()
        .map(|plan| export_plan(conn, plan))
        .collect::<anyhow::Result<_>>()?;
    Ok(Snapshot {
        version: SNAPSHOT_VERSION,
        plans,
    })
}

fn export_plan(conn: &Connection, plan: Plan) -> anyhow::Result<PlanEntry> {
    let timestamps: (String, String) = conn.query_row(
        "SELECT created_at, updated_at FROM plans WHERE name = ?1",
        [&plan.name],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let todos = db::todos_of(conn, &plan.name)?
        .into_iter()
        .map(|t| TodoEntry {
            id: t.id,
            text: t.text,
            status: t.status,
            order: t.sort_order,
        })
        .collect();
    let notes = db::notes_of(conn, &plan.name)?
        .into_iter()
        .map(|n| NoteEntry {
            id: n.id,
            text: n.text,
            created_at: n.created_at,
        })
        .collect();
    Ok(PlanEntry {
        depends_on: db::deps_of(conn, &plan.name)?,
        todos,
        notes,
        created_at: timestamps.0,
        updated_at: timestamps.1,
        name: plan.name,
        title: plan.title,
        branch: plan.branch,
        status: plan.status,
        order: plan.sort_order,
        merge_commit: plan.merge_commit,
        goal: plan.goal,
        context: plan.context,
        definition_of_done: plan.definition_of_done,
        review_type: plan.review_type,
    })
}

/// Serialize to byte-stable YAML.
pub fn to_yaml(snapshot: &Snapshot) -> anyhow::Result<String> {
    serde_yaml::to_string(snapshot).context("serialize snapshot")
}

/// Replace the registry with a snapshot's content. Without `force` the
/// target registry must be empty. Multi-statement, so one immediate
/// transaction wraps the whole import.
pub fn import(conn: &Connection, snapshot: &Snapshot, force: bool) -> anyhow::Result<usize> {
    if snapshot.version != SNAPSHOT_VERSION {
        bail!(
            "unsupported snapshot version {} (expected {SNAPSHOT_VERSION})",
            snapshot.version
        );
    }
    let existing = db::list(conn, None)?.len();
    if existing > 0 && !force {
        bail!("registry holds {existing} plans; pass --force to replace it");
    }

    // Insert every plan first: edges, todos, and notes all reference them.
    db::with_immediate(conn, |conn| {
        conn.execute("DELETE FROM plan_notes", [])?;
        conn.execute("DELETE FROM plan_todos", [])?;
        conn.execute("DELETE FROM plan_deps", [])?;
        conn.execute("DELETE FROM plans", [])?;
        for entry in &snapshot.plans {
            let plan = Plan {
                name: entry.name.clone(),
                title: entry.title.clone(),
                branch: entry.branch.clone(),
                status: entry.status.clone(),
                sort_order: entry.order,
                merge_commit: entry.merge_commit.clone(),
                goal: entry.goal.clone(),
                context: entry.context.clone(),
                definition_of_done: entry.definition_of_done.clone(),
                review_type: entry.review_type.clone(),
            };
            // Reuses the registry's own validation for name, status, and
            // review_type; timestamps restore afterwards.
            db::insert(conn, &plan)?;
            conn.execute(
                "UPDATE plans SET created_at = ?1, updated_at = ?2 WHERE name = ?3",
                rusqlite::params![entry.created_at, entry.updated_at, entry.name],
            )?;
            for todo in &entry.todos {
                conn.execute(
                    "INSERT INTO plan_todos(id, plan, text, status, sort_order)
                     VALUES(?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![todo.id, entry.name, todo.text, todo.status, todo.order],
                )?;
            }
            for note in &entry.notes {
                conn.execute(
                    "INSERT INTO plan_notes(id, plan, text, created_at)
                     VALUES(?1, ?2, ?3, ?4)",
                    rusqlite::params![note.id, entry.name, note.text, note.created_at],
                )?;
            }
        }
        for entry in &snapshot.plans {
            db::set_deps(conn, &entry.name, &entry.depends_on)?;
        }
        resync_sequence(conn, "plan_todos")?;
        resync_sequence(conn, "plan_notes")?;
        Ok(())
    })?;
    Ok(snapshot.plans.len())
}

/// Point the AUTOINCREMENT counter past the restored ids. REPLACE handles
/// the fresh-database case where no sequence row exists yet.
fn resync_sequence(conn: &Connection, table: &str) -> anyhow::Result<()> {
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO sqlite_sequence(name, seq)
             SELECT '{table}', COALESCE(MAX(id), 0) FROM {table}"
        ),
        [],
    )?;
    Ok(())
}
