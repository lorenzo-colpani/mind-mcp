//! MCP tool surface. Eleven tools: two plans-read, four plans-write, one
//! maintenance, four brain. Mutating tools write through to plans.md and
//! plans.yaml.

use std::fmt::Write as _;

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};
use serde::Deserialize;

use crate::db::{self, Patch, Plan};
use crate::markdown;
use crate::state::{Project, valid_name};

fn tool_err(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(msg.into())])
}

fn tool_ok(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(msg.into())])
}

/// Serializes mutating tool calls in arrival order (FIFO-fair lock). The
/// server answers requests concurrently, and every mutation is
/// read-check-then-write across its own connection.
static WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn conn_of(project: &Project) -> Result<rusqlite::Connection, CallToolResult> {
    db::open(&project.db_path().map_err(|e| tool_err(e.to_string()))?)
        .map_err(|e| tool_err(e.to_string()))
}

// ---------- rendering helpers ----------

fn render_board(plans: &[db::Plan], deps: &[(String, String)]) -> String {
    let mut out =
        String::from("| Name | Status | Order | Depends on | Goal |\n|---|---|---|---|---|\n");
    for p in plans {
        let dep_list: Vec<&str> = deps
            .iter()
            .filter(|(a, _)| a == &p.name)
            .map(|(_, b)| b.as_str())
            .collect();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            p.name,
            p.status,
            p.sort_order,
            if dep_list.is_empty() {
                "—".into()
            } else {
                dep_list.join(", ")
            },
            p.title
        );
    }
    out
}

/// Depth-first tree over `dependent` edges. Roots: plans with no dependencies.
/// Each plan prints once, under its first parent.
fn render_tree(plans: &[Plan], edges: &[(String, String)]) -> String {
    let children: Vec<(String, Vec<String>)> = plans
        .iter()
        .map(|p| {
            let kids: Vec<String> = edges
                .iter()
                .filter(|(a, _)| a == &p.name)
                .map(|(_, b)| b.clone())
                .collect();
            (p.name.clone(), kids)
        })
        .collect();

    let mut printed: Vec<String> = Vec::new();
    let mut out = String::new();

    fn walk(
        name: &str,
        depth: usize,
        children: &[(String, Vec<String>)],
        printed: &mut Vec<String>,
        out: &mut String,
    ) {
        if printed.iter().any(|p| p == name) {
            return;
        }
        printed.push(name.to_string());
        let _ = writeln!(out, "{}{}", "  ".repeat(depth), name);
        // Dependents of `name`, in board order.
        let dependents: Vec<&String> = children
            .iter()
            .filter(|(_, kids)| kids.iter().any(|k| k == name))
            .map(|(parent, _)| parent)
            .collect();
        for d in dependents {
            walk(d, depth + 1, children, printed, out);
        }
    }

    for (name, kids) in &children {
        if kids.is_empty() {
            walk(name, 0, &children, &mut printed, &mut out);
        }
    }
    // Plans inside a cycle never print above; flush them.
    for (name, _) in &children {
        if !printed.iter().any(|p| p == name) {
            walk(name, 0, &children, &mut printed, &mut out);
        }
    }
    out
}

fn render_mermaid(edges: &[(String, String)]) -> String {
    if edges.is_empty() {
        return "(no dependencies)\n".to_string();
    }
    let mut out = String::from("graph TD\n");
    for (plan, dep) in edges {
        let _ = writeln!(out, "  {plan}-->|depends on|{dep}");
    }
    out
}

fn render_detail(p: &Plan, deps: &[String], dependents: &[String]) -> String {
    let fmt_list = |items: &[String]| {
        if items.is_empty() {
            "—".to_string()
        } else {
            items.join(", ")
        }
    };
    format!(
        "# {}\n\n- status: {}\n- branch: {}\n- order: {}\n- progress: {}\n- merge_commit: {}\n- depends on: {}\n- blocks: {}\n",
        p.name,
        p.status,
        if p.branch.is_empty() {
            "—"
        } else {
            &p.branch
        },
        p.sort_order,
        if p.progress.is_empty() {
            "—"
        } else {
            &p.progress
        },
        if p.merge_commit.is_empty() {
            "—"
        } else {
            &p.merge_commit
        },
        fmt_list(deps),
        fmt_list(dependents)
    )
}

// ---------- shared logic (used by both the CLI and MCP handlers) ----------

pub fn show_impl(
    project: &Project,
    name: Option<&str>,
    format: Option<String>,
) -> anyhow::Result<String> {
    let conn = db::open(&project.db_path()?)?;

    if let Some(name) = name {
        let Some(p) = db::get(&conn, name)? else {
            anyhow::bail!("unknown plan '{name}'");
        };
        let deps = db::deps_of(&conn, name)?;
        let dependents = db::dependents_of(&conn, name)?;
        return Ok(render_detail(&p, &deps, &dependents));
    }

    let plans = db::list(&conn, None)?;
    let edges = db::all_edges(&conn)?;
    match format.as_deref().unwrap_or("board") {
        "tree" => Ok(render_tree(&plans, &edges)),
        "mermaid" => Ok(render_mermaid(&edges)),
        _ => Ok(render_board(&plans, &edges)),
    }
}

pub fn ready_impl(project: &Project) -> anyhow::Result<String> {
    let conn = db::open(&project.db_path()?)?;
    let ready = db::ready(&conn)?;
    if ready.is_empty() {
        return Ok("Nothing is unblocked right now.".to_string());
    }
    Ok(render_board(&ready, &db::all_edges(&conn)?))
}

/// Every mutating plans tool funnels here: mutate, then write through.
fn after_mutation(project: &Project) -> String {
    match (
        markdown::sync_plans_md(project),
        markdown::export_yaml(project),
    ) {
        (Ok(()), Ok(())) => "\n(synced: plans.md + plans.yaml)".to_string(),
        (Err(e), _) | (_, Err(e)) => format!("\n(warning: sync failed: {e})"),
    }
}

// ---------- MCP tool definitions ----------

#[derive(Clone)]
pub struct MindTools {
    pub project: Project,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ShowArgs {
    /// Plan name. Omit to show the whole board.
    pub name: Option<String>,
    /// Board view without a name: 'board' (default), 'tree', or 'mermaid'.
    pub format: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    pub name: String,
    pub title: String,
    pub branch: Option<String>,
    /// Position in run order. Default: after the last plan.
    pub order: Option<i64>,
    /// Also create plans/<name>/README.md and discussion.md from templates.
    pub scaffold: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct UpdateArgs {
    pub name: String,
    pub title: Option<String>,
    pub branch: Option<String>,
    pub status: Option<String>,
    pub progress: Option<String>,
    pub merge_commit: Option<String>,
    pub order: Option<i64>,
    /// Replaces the whole dependency list. Omit to leave unchanged.
    pub depends_on: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct NameArgs {
    pub name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RenameArgs {
    pub name: String,
    /// New unique plan name.
    pub new_name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct BrainAddArgs {
    /// Topic tag, e.g. rust, git, sql, workflow.
    pub tag: String,
    /// One or two short sentences. Active voice.
    pub lesson: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct BrainListArgs {
    /// Filter by tag. Omit for all lessons.
    pub tag: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct BrainEditArgs {
    pub id: i64,
    pub text: Option<String>,
    pub tag: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct BrainIdArgs {
    pub id: i64,
}

const README_TEMPLATE: &str = "\
# {name}

**Branch:** `{branch}`
**Status:** see the mind-mcp board

## Goal

## Steps

## Definition of done
";

const DISCUSSION_TEMPLATE: &str = "\
# {name} — Discussion

Log decisions and open points here. Append as you work.
";

#[tool_router(server_handler)]
impl MindTools {
    #[tool(
        description = "Show plan state. Without arguments: the whole board. With format=tree or mermaid: the dependency graph. With name: full record of one plan plus its dependencies and dependents."
    )]
    async fn plans_show(&self, Parameters(args): Parameters<ShowArgs>) -> CallToolResult {
        match show_impl(&self.project, args.name.as_deref(), args.format) {
            Ok(out) => tool_ok(out),
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(
        description = "List pending plans whose every dependency is done — what can start right now."
    )]
    async fn plans_ready(&self) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        match ready_impl(&self.project) {
            Ok(out) => tool_ok(out),
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(
        description = "Add a plan. Optionally scaffolds plans/<name>/README.md and discussion.md. Syncs plans.md and plans.yaml."
    )]
    async fn plans_add(&self, Parameters(args): Parameters<AddArgs>) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        let conn = match conn_of(&self.project) {
            Ok(c) => c,
            Err(e) => return e,
        };
        if !valid_name(&args.name) {
            return tool_err(format!(
                "invalid name '{}': use lowercase letters, digits, hyphens",
                args.name
            ));
        }
        let plan = Plan {
            name: args.name.clone(),
            title: args.title.clone(),
            branch: args.branch.unwrap_or_default(),
            status: "pending".into(),
            progress: String::new(),
            sort_order: match args.order {
                Some(o) => o,
                None => match db::next_order(&conn) {
                    Ok(n) => n,
                    Err(e) => return tool_err(e.to_string()),
                },
            },
            merge_commit: String::new(),
        };
        if let Err(e) = db::insert(&conn, &plan) {
            return tool_err(e.to_string());
        }
        if args.scaffold.unwrap_or(false) {
            let dir = self.project.plan_dir(&args.name);
            let readme = README_TEMPLATE
                .replace("{name}", &args.name)
                .replace("{branch}", &plan.branch);
            if let Err(e) = crate::state::write_file(&dir.join("README.md"), &readme) {
                return tool_err(format!("scaffold failed: {e}"));
            }
            if let Err(e) = crate::state::write_file(
                &dir.join("discussion.md"),
                &DISCUSSION_TEMPLATE.replace("{name}", &args.name),
            ) {
                return tool_err(format!("scaffold failed: {e}"));
            }
        }
        tool_ok(format!(
            "added '{}' (order {}){}",
            args.name,
            plan.sort_order,
            after_mutation(&self.project)
        ))
    }

    #[tool(
        description = "Update a plan. Only provided fields change. depends_on replaces the whole dependency list (omit to keep). Rejects dependency cycles. Syncs plans.md and plans.yaml."
    )]
    async fn plans_update(&self, Parameters(args): Parameters<UpdateArgs>) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        let conn = match conn_of(&self.project) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let patch = Patch {
            title: args.title.as_deref(),
            branch: args.branch.as_deref(),
            status: args.status.as_deref(),
            progress: args.progress.as_deref(),
            order: args.order,
            merge_commit: args.merge_commit.as_deref(),
        };
        if let Err(e) = db::update(&conn, &args.name, &patch) {
            return tool_err(e.to_string());
        }
        if let Some(deps) = &args.depends_on
            && let Err(e) = db::set_deps(&conn, &args.name, deps)
        {
            return tool_err(e.to_string());
        }
        tool_ok(format!("updated '{}'", args.name) + &after_mutation(&self.project))
    }

    #[tool(
        description = "Rename a plan. Dependency edges follow, both directions. Does not touch a plans/<name>/ folder — rename that yourself if one exists. Syncs plans.md and plans.yaml."
    )]
    async fn plans_rename(&self, Parameters(args): Parameters<RenameArgs>) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        let conn = match conn_of(&self.project) {
            Ok(c) => c,
            Err(e) => return e,
        };
        if let Err(e) = db::rename(&conn, &args.name, &args.new_name) {
            return tool_err(e.to_string());
        }
        tool_ok(
            format!("renamed '{}' -> '{}'", args.name, args.new_name)
                + &after_mutation(&self.project),
        )
    }

    #[tool(
        description = "Delete a plan and its dependency links. Does not touch the plans/<name>/ folder. Syncs plans.md and plans.yaml."
    )]
    async fn plans_delete(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        let conn = match conn_of(&self.project) {
            Ok(c) => c,
            Err(e) => return e,
        };
        if let Err(e) = db::delete(&conn, &args.name) {
            return tool_err(e.to_string());
        }
        tool_ok(format!("deleted '{}'", args.name) + &after_mutation(&self.project))
    }

    #[tool(description = "Regenerate plans.yaml from the database.")]
    async fn plans_export(&self) -> CallToolResult {
        match markdown::export_yaml(&self.project) {
            Ok(()) => tool_ok("plans.yaml written"),
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(
        description = "Add a global lesson to the brain file (~/.config/opencode/brain.md). Cross-project lessons only; repo-specific findings belong in that repo's docs/FINDINGS.md."
    )]
    async fn brain_add(&self, Parameters(args): Parameters<BrainAddArgs>) -> CallToolResult {
        match markdown::brain_add(&args.tag, &args.lesson) {
            Ok(l) => tool_ok(format!("added lesson id {}: [{}] {}", l.id, l.tag, l.text)),
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(description = "List brain lessons, optionally filtered by tag.")]
    async fn brain_list(&self, Parameters(args): Parameters<BrainListArgs>) -> CallToolResult {
        match markdown::read_brain() {
            Ok(lessons) => {
                let filtered: Vec<_> = lessons
                    .iter()
                    .filter(|l| args.tag.as_ref().is_none_or(|t| l.tag == *t))
                    .collect();
                if filtered.is_empty() {
                    return tool_ok("(no lessons)");
                }
                let body: String = filtered
                    .iter()
                    .map(|l| format!("- [{}] {} (id {})", l.tag, l.text, l.id))
                    .collect::<Vec<_>>()
                    .join("\n");
                tool_ok(body)
            }
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(description = "Edit a brain lesson's text or tag by id.")]
    async fn brain_edit(&self, Parameters(args): Parameters<BrainEditArgs>) -> CallToolResult {
        match markdown::brain_edit(args.id, args.tag.as_deref(), args.text.as_deref()) {
            Ok(l) => tool_ok(format!("updated id {}: [{}] {}", l.id, l.tag, l.text)),
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(description = "Remove a brain lesson by id.")]
    async fn brain_remove(&self, Parameters(args): Parameters<BrainIdArgs>) -> CallToolResult {
        match markdown::brain_remove(args.id) {
            Ok(()) => tool_ok(format!("removed id {}", args.id)),
            Err(e) => tool_err(e.to_string()),
        }
    }
}
