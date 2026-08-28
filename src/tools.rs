//! MCP tool surface. Ten plans tools + four brain tools. The registry is the
//! committed `plans.db`; nothing is generated or synced.

use std::fmt::Write as _;

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};
use serde::Deserialize;

use crate::db::{self, Patch, Plan};
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
    db::open(&project.db_path()).map_err(|e| tool_err(e.to_string()))
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

/// One plan in full: record fields, then Goal / Context / Definition of done
/// / Steps / Notes sections. Empty sections are omitted.
pub fn render_detail(
    p: &Plan,
    deps: &[String],
    dependents: &[String],
    todos: &[db::Todo],
    notes: &[db::Note],
) -> String {
    let fmt_list = |items: &[String]| {
        if items.is_empty() {
            "—".to_string()
        } else {
            items.join(", ")
        }
    };
    let mut out = format!("# {} — {}\n\n", p.name, p.title);
    let _ = write!(out, "- status: {}\n- review: {}\n", p.status, p.review_type);
    if !p.branch.is_empty() {
        let _ = writeln!(out, "- branch: {}", p.branch);
    }
    let _ = writeln!(out, "- order: {}", p.sort_order);
    if !p.merge_commit.is_empty() {
        let _ = writeln!(out, "- merge_commit: {}", p.merge_commit);
    }
    let _ = write!(
        out,
        "- depends on: {}\n- blocks: {}\n",
        fmt_list(deps),
        fmt_list(dependents)
    );

    let mut sect = |title: &str, body: &str| {
        if !body.trim().is_empty() {
            let _ = write!(out, "\n## {title}\n\n{}\n", body.trim());
        }
    };
    sect("Goal", &p.goal);
    sect("Context", &p.context);
    sect("Definition of done", &p.definition_of_done);

    if !todos.is_empty() {
        out.push_str("\n## Steps\n\n");
        for t in todos {
            match t.status.as_str() {
                "done" => {
                    let _ = writeln!(out, "- [x] {} (id {})", t.text, t.id);
                }
                "in_progress" => {
                    let _ = writeln!(out, "- [ ] {} (id {}, in_progress)", t.text, t.id);
                }
                _ => {
                    let _ = writeln!(out, "- [ ] {} (id {})", t.text, t.id);
                }
            }
        }
    }

    if !notes.is_empty() {
        out.push_str("\n## Notes\n\n");
        for n in notes {
            let _ = writeln!(out, "- {} (id {})", n.text.replace('\n', "\n  "), n.id);
        }
    }
    out
}

// ---------- shared logic (used by both the CLI and MCP handlers) ----------

pub fn show_impl(
    project: &Project,
    name: Option<&str>,
    format: Option<String>,
) -> anyhow::Result<String> {
    let conn = db::open(&project.db_path())?;

    if let Some(name) = name {
        let Some(p) = db::get(&conn, name)? else {
            anyhow::bail!("unknown plan '{name}'");
        };
        let deps = db::deps_of(&conn, name)?;
        let dependents = db::dependents_of(&conn, name)?;
        let todos = db::todos_of(&conn, name)?;
        let notes = db::notes_of(&conn, name)?;
        return Ok(render_detail(&p, &deps, &dependents, &todos, &notes));
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
    let conn = db::open(&project.db_path())?;
    let ready = db::ready(&conn)?;
    if ready.is_empty() {
        return Ok("Nothing is unblocked right now.".to_string());
    }
    Ok(render_board(&ready, &db::all_edges(&conn)?))
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
    /// The outcome, one paragraph.
    pub goal: Option<String>,
    /// Background: why now, constraints, links.
    pub context: Option<String>,
    /// What "done" must prove.
    pub definition_of_done: Option<String>,
    /// Review gate: 'deep', 'quick', or 'none'. Default: deep.
    pub review_type: Option<String>,
    pub depends_on: Option<Vec<String>>,
    /// Position in run order. Default: after the last plan.
    pub order: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct UpdateArgs {
    pub name: String,
    pub title: Option<String>,
    pub branch: Option<String>,
    pub status: Option<String>,
    pub merge_commit: Option<String>,
    pub goal: Option<String>,
    pub context: Option<String>,
    pub definition_of_done: Option<String>,
    pub review_type: Option<String>,
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
pub struct TodoAddArgs {
    pub plan: String,
    pub text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TodoEditArgs {
    pub id: i64,
    pub text: Option<String>,
    /// pending, in_progress, or done.
    pub status: Option<String>,
    /// Position within the plan's steps.
    pub order: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TodoIdArgs {
    pub id: i64,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct NoteAddArgs {
    pub plan: String,
    /// Decision, finding, or open point. Appended to the plan's log.
    pub text: String,
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

#[tool_router(server_handler)]
impl MindTools {
    #[tool(
        description = "Show plan state. Without arguments: the whole board. With format=tree or mermaid: the dependency graph. With name: full record of one plan — goal, context, definition of done, todos, notes — plus its dependencies and dependents."
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
        description = "Add a plan with its definition: title, goal, context, definition of done, review type (deep|quick|none). Steps usually follow via plans_todo_add."
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
        if let Some(deps) = &args.depends_on
            && let Err(e) = check_deps(&conn, &args.name, deps)
        {
            return tool_err(e.to_string());
        }
        let order = db::with_immediate(&conn, |conn| {
            let sort_order = match args.order {
                Some(o) => o,
                None => db::next_order(conn)?,
            };
            let plan = db::Plan {
                name: args.name.clone(),
                title: args.title.clone(),
                branch: args.branch.clone().unwrap_or_default(),
                status: "pending".into(),
                sort_order,
                merge_commit: String::new(),
                goal: args.goal.clone().unwrap_or_default(),
                context: args.context.clone().unwrap_or_default(),
                definition_of_done: args.definition_of_done.clone().unwrap_or_default(),
                review_type: args.review_type.clone().unwrap_or_else(|| "deep".into()),
            };
            db::insert(conn, &plan)?;
            if let Some(deps) = &args.depends_on {
                db::set_deps(conn, &args.name, deps)?;
            }
            Ok(sort_order)
        });
        match order {
            Ok(order) => tool_ok(format!("added '{}' (order {})", args.name, order)),
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(
        description = "Update a plan. Only provided fields change. depends_on replaces the whole dependency list (omit to keep). Rejects dependency cycles."
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
            order: args.order,
            merge_commit: args.merge_commit.as_deref(),
            goal: args.goal.as_deref(),
            context: args.context.as_deref(),
            definition_of_done: args.definition_of_done.as_deref(),
            review_type: args.review_type.as_deref(),
        };
        let result = db::with_immediate(&conn, |conn| {
            db::update(conn, &args.name, &patch)?;
            if let Some(deps) = &args.depends_on {
                db::set_deps(conn, &args.name, deps)?;
            }
            Ok(())
        });
        match result {
            Ok(()) => tool_ok(format!("updated '{}'", args.name)),
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(description = "Rename a plan. Dependency edges, todos, and notes follow.")]
    async fn plans_rename(&self, Parameters(args): Parameters<RenameArgs>) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        let conn = match conn_of(&self.project) {
            Ok(c) => c,
            Err(e) => return e,
        };
        if let Err(e) = db::rename(&conn, &args.name, &args.new_name) {
            return tool_err(e.to_string());
        }
        tool_ok(format!("renamed '{}' -> '{}'", args.name, args.new_name))
    }

    #[tool(description = "Delete a plan and its dependency links, todos, and notes.")]
    async fn plans_delete(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        let conn = match conn_of(&self.project) {
            Ok(c) => c,
            Err(e) => return e,
        };
        if let Err(e) = db::delete(&conn, &args.name) {
            return tool_err(e.to_string());
        }
        tool_ok(format!("deleted '{}'", args.name))
    }

    #[tool(description = "Add a step (todo) to a plan. Returns the todo id.")]
    async fn plans_todo_add(&self, Parameters(args): Parameters<TodoAddArgs>) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        match conn_of(&self.project) {
            Ok(conn) => match db::todo_add(&conn, &args.plan, &args.text) {
                Ok(id) => tool_ok(format!("todo {id} added to '{}'", args.plan)),
                Err(e) => tool_err(e.to_string()),
            },
            Err(e) => e,
        }
    }

    #[tool(
        description = "Edit a todo: text, status (pending, in_progress, done), or order. Only provided fields change."
    )]
    async fn plans_todo_edit(&self, Parameters(args): Parameters<TodoEditArgs>) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        let conn = match conn_of(&self.project) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let patch = db::TodoPatch {
            text: args.text.as_deref(),
            status: args.status.as_deref(),
            order: args.order,
        };
        match db::todo_edit(&conn, args.id, &patch) {
            Ok(()) => tool_ok(format!("todo {} updated", args.id)),
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(description = "Remove a todo by id.")]
    async fn plans_todo_remove(&self, Parameters(args): Parameters<TodoIdArgs>) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        match conn_of(&self.project) {
            Ok(conn) => match db::todo_remove(&conn, args.id) {
                Ok(()) => tool_ok(format!("todo {} removed", args.id)),
                Err(e) => tool_err(e.to_string()),
            },
            Err(e) => e,
        }
    }

    #[tool(
        description = "Append a note to a plan's log: decisions, findings, open points, what happened. Append-only."
    )]
    async fn plans_note_add(&self, Parameters(args): Parameters<NoteAddArgs>) -> CallToolResult {
        let _guard = WRITE_LOCK.lock().await;
        match conn_of(&self.project) {
            Ok(conn) => match db::note_add(&conn, &args.plan, &args.text) {
                Ok(id) => tool_ok(format!("note {id} added to '{}'", args.plan)),
                Err(e) => tool_err(e.to_string()),
            },
            Err(e) => e,
        }
    }

    #[tool(
        description = "Add a global lesson to the brain file (~/.config/opencode/brain.md). Cross-project lessons only; repo-specific findings belong in that repo's docs/FINDINGS.md."
    )]
    async fn brain_add(&self, Parameters(args): Parameters<BrainAddArgs>) -> CallToolResult {
        match crate::brain::brain_add(&args.tag, &args.lesson) {
            Ok(l) => tool_ok(format!("added lesson id {}: [{}] {}", l.id, l.tag, l.text)),
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(description = "List brain lessons, optionally filtered by tag.")]
    async fn brain_list(&self, Parameters(args): Parameters<BrainListArgs>) -> CallToolResult {
        match crate::brain::read_brain() {
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
        match crate::brain::brain_edit(args.id, args.tag.as_deref(), args.text.as_deref()) {
            Ok(l) => tool_ok(format!("updated id {}: [{}] {}", l.id, l.tag, l.text)),
            Err(e) => tool_err(e.to_string()),
        }
    }

    #[tool(description = "Remove a brain lesson by id.")]
    async fn brain_remove(&self, Parameters(args): Parameters<BrainIdArgs>) -> CallToolResult {
        match crate::brain::brain_remove(args.id) {
            Ok(()) => tool_ok(format!("removed id {}", args.id)),
            Err(e) => tool_err(e.to_string()),
        }
    }
}

/// Validate dependencies before a plan insert: known targets, no self-dep.
fn check_deps(conn: &rusqlite::Connection, plan: &str, deps: &[String]) -> Result<(), String> {
    if deps.iter().any(|d| d == plan) {
        return Err("plan cannot depend on itself".to_string());
    }
    for d in deps {
        match db::get(conn, d) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(format!("unknown dependency plan '{d}'")),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
