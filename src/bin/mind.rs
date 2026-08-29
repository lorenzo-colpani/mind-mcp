//! Human-facing CLI over the mind registry: browse plans, the dependency
//! tree, and mutate entries exactly like the MCP tools do — same committed
//! plans.db, no drift.

use anyhow::Context;
use clap::{Parser, Subcommand};
use mind_mcp::{adopt, db, state::Project, tools};

#[derive(Parser)]
#[command(name = "mind", about = "Plan registry CLI for humans")]
struct Cli {
    /// Emit machine-readable JSON where supported.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Plans by run order. Defaults to work in flight; finished plans need
    /// --all or an explicit --status.
    Board {
        /// Only plans with this status.
        #[arg(long)]
        status: Option<String>,
        /// Include finished work.
        #[arg(long)]
        all: bool,
    },
    /// Dependency graph. Without a plan: the active work, skipping edges
    /// between finished plans. With a plan: its direct dependencies and
    /// dependents, whatever their status.
    Tree {
        /// Focus on one plan (one hop each direction).
        plan: Option<String>,
        #[arg(long, default_value = "mermaid")]
        format: String,
        /// Include finished work.
        #[arg(long)]
        all: bool,
    },
    /// One plan in full: goal, context, definition of done, todos, notes.
    Show { name: String },
    /// Pending plans whose every dependency is done.
    Ready,
    Add {
        name: String,
        title: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        goal: Option<String>,
        #[arg(long)]
        context: Option<String>,
        #[arg(long = "definition-of-done")]
        definition_of_done: Option<String>,
        #[arg(long = "review", default_value = "deep")]
        review_type: String,
        #[arg(long, default_value = "pending")]
        status: String,
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
        #[arg(long)]
        after: Option<i64>,
    },
    Update {
        name: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        merge_commit: Option<String>,
        #[arg(long)]
        goal: Option<String>,
        #[arg(long)]
        context: Option<String>,
        #[arg(long = "definition-of-done")]
        definition_of_done: Option<String>,
        #[arg(long = "review")]
        review_type: Option<String>,
        /// Clears the whole dependency list.
        #[arg(long = "clear-deps")]
        clear_deps: bool,
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
    },
    /// Renames a plan; dependency edges, todos, and notes follow.
    Rename { name: String, new_name: String },
    /// Deletes a plan and its dependency links, todos, and notes.
    Remove { name: String },
    /// Per-plan steps.
    Todo {
        #[command(subcommand)]
        cmd: TodoCmd,
    },
    /// Appends a note to a plan's log.
    Note { plan: String, text: String },
    /// One-time migration into the committed plans.db: legacy hidden DB,
    /// plans/ folders, plans.md, plans.yaml.
    Adopt,
}

#[derive(Subcommand)]
enum TodoCmd {
    /// Todos by status: open work by default, done with --all.
    List {
        plan: String,
        /// Only todos with this status: pending, in_progress, or done.
        #[arg(long)]
        status: Option<String>,
        /// Include done todos.
        #[arg(long)]
        all: bool,
    },
    Add {
        plan: String,
        text: String,
    },
    Edit {
        id: i64,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        order: Option<i64>,
    },
    Remove {
        id: i64,
    },
}

fn glyphs(status: &str) -> &'static str {
    match status {
        "done" => "\u{2713}",        // check
        "in_progress" => "\u{25cf}", // filled circle
        "partial" => "\u{25d0}",     // half circle
        _ => "\u{25cb}",             // empty circle
    }
}

fn open_project() -> anyhow::Result<(Project, rusqlite::Connection)> {
    let project = Project::resolve()?;
    let path = project.db_path();
    let conn = db::open(&path).with_context(|| format!("open registry at {}", path.display()))?;
    Ok((project, conn))
}

fn main() {
    // Piping into head/less closes stdout early; swallow exactly that panic
    // and keep the loud hook for every other failure.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let broken = info
            .payload()
            .downcast_ref::<String>()
            .is_some_and(|message| message.contains("Broken pipe"))
            || info
                .payload()
                .downcast_ref::<&str>()
                .is_some_and(|message| message.contains("Broken pipe"));
        if !broken {
            default_hook(info);
        }
    }));
    match std::panic::catch_unwind(real_main) {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            eprintln!("Error: {err:?}");
            std::process::exit(1);
        }
        Err(_) => std::process::exit(1),
    }
}

fn real_main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Adopt must run before anything opens (and thereby creates) plans.db.
    if let Cmd::Adopt = &cli.cmd {
        let project = Project::resolve()?;
        println!("{}", adopt::run(&project, &adopt::default_legacy_dir())?);
        return Ok(());
    }

    let (project, conn) = open_project()?;

    match &cli.cmd {
        Cmd::Board { status, all } => {
            let mut plans = db::list(&conn, status.as_deref())?;
            if status.is_none() && !*all {
                let total = plans.len();
                plans.retain(|p| p.status != "done");
                let hidden = total - plans.len();
                if hidden > 0 {
                    println!("{hidden} done plans hidden; use --all to show them\n");
                }
            }
            plans.sort_by_key(|p| p.sort_order);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&plans)?);
                return Ok(());
            }
            println!(
                "{:<4} {:<24} {:<12} {:<22} TITLE",
                "#", "NAME", "STATUS", "DEPENDS ON"
            );
            for p in &plans {
                let deps = db::deps_of(&conn, &p.name)?.join(",");
                let deps = if deps.len() > 20 {
                    format!("{}...", &deps[..20])
                } else {
                    deps
                };
                println!(
                    "{:<4} {} {:<24} {:<22} {}",
                    p.sort_order,
                    glyphs(&p.status),
                    p.name,
                    deps,
                    p.title
                );
            }
            let done = plans.iter().filter(|p| p.status == "done").count();
            println!(
                "\n{} plans: {} done, {} active, {} pending",
                plans.len(),
                done,
                plans
                    .iter()
                    .filter(|p| p.status != "done" && p.status != "pending")
                    .count(),
                plans.iter().filter(|p| p.status == "pending").count(),
            );
        }

        Cmd::Tree { plan, format, all } => {
            let plans = db::list(&conn, None)?;
            let glyph_of = |name: &str| -> String {
                match plans.iter().find(|p| p.name == name) {
                    Some(p) => glyphs(&p.status).to_string(),
                    None => String::new(),
                }
            };

            let mut edges: Vec<(String, String)> = db::all_edges(&conn)?
                .into_iter()
                .filter(|(plan_name, dep)| {
                    if *all {
                        return true;
                    }
                    match plan.as_deref() {
                        // Focused mode keeps every edge touching the plan.
                        Some(focus) => plan_name == focus || dep == focus,
                        // Whole-graph mode drops finished-to-finished edges.
                        None => {
                            let done = |n: &str| glyph_of(n) == "\u{2713}";
                            !(done(plan_name) && done(dep))
                        }
                    }
                })
                .collect();
            if let Some(focus) = plan.as_deref() {
                if db::get(&conn, focus)?.is_none() {
                    anyhow::bail!("unknown plan '{focus}'");
                }
                edges.retain(|(plan_name, dep)| plan_name == focus || dep == focus);
            }

            if format == "ascii" {
                for (dependent, dep) in &edges {
                    println!(
                        "{} {} --> {} {}",
                        glyph_of(dep),
                        dep,
                        glyph_of(dependent),
                        dependent
                    );
                }
            } else {
                println!("flowchart TD");
                for (dependent, dep) in &edges {
                    println!(
                        "    \"{dep}\"[\"{} {dep}\"] --> \"{dependent}\"[\"{} {dependent}\"]",
                        glyph_of(dep),
                        glyph_of(dependent)
                    );
                }
            }
        }

        Cmd::Show { name } => {
            let detail = tools::detail_impl(&project, name)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&detail)?);
            } else {
                println!(
                    "{}",
                    tools::render_detail(
                        &detail.plan,
                        &detail.depends_on,
                        &detail.dependents,
                        &detail.todos,
                        &detail.notes,
                    )
                );
            }
        }

        Cmd::Ready => {
            let plans = db::ready(&conn)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&plans)?);
                return Ok(());
            }
            for p in &plans {
                println!("{} {} — {}", glyphs(&p.status), p.name, p.title);
            }
            if plans.is_empty() {
                println!("nothing ready; everything is blocked or done");
            }
        }

        Cmd::Add {
            name,
            title,
            branch,
            goal,
            context,
            definition_of_done,
            review_type,
            status,
            depends_on,
            after,
        } => {
            // Validate everything before writing: a rejected add must not
            // leave a half-created plan behind.
            for dep in depends_on {
                if db::get(&conn, dep)?.is_none() {
                    anyhow::bail!("unknown dependency plan '{dep}'");
                }
            }
            db::with_immediate(&conn, |conn| {
                let order = match after {
                    Some(prev) => prev + 1,
                    None => db::next_order(conn)?,
                };
                let plan = db::Plan {
                    name: name.clone(),
                    title: title.clone(),
                    branch: branch.clone().unwrap_or_default(),
                    status: status.clone(),
                    sort_order: order,
                    merge_commit: String::new(),
                    goal: goal.clone().unwrap_or_default(),
                    context: context.clone().unwrap_or_default(),
                    definition_of_done: definition_of_done.clone().unwrap_or_default(),
                    review_type: review_type.clone(),
                };
                db::insert(conn, &plan)?;
                if !depends_on.is_empty() {
                    db::set_deps(conn, name, depends_on)?;
                }
                Ok(())
            })?;
            println!("added {name}");
        }

        Cmd::Update {
            name,
            status,
            branch,
            merge_commit,
            goal,
            context,
            definition_of_done,
            review_type,
            clear_deps,
            depends_on,
        } => {
            let patch = db::Patch {
                title: None,
                branch: branch.as_deref(),
                status: status.as_deref(),
                order: None,
                merge_commit: merge_commit.as_deref(),
                goal: goal.as_deref(),
                context: context.as_deref(),
                definition_of_done: definition_of_done.as_deref(),
                review_type: review_type.as_deref(),
            };
            let deps: Option<Vec<String>> = if *clear_deps {
                Some(Vec::new())
            } else if depends_on.is_empty() {
                None
            } else {
                Some(depends_on.clone())
            };
            db::with_immediate(&conn, |conn| {
                db::update(conn, name, &patch)?;
                if let Some(deps) = deps {
                    db::set_deps(conn, name, &deps)?;
                }
                Ok(())
            })?;
            println!("updated {name}");
        }

        Cmd::Rename { name, new_name } => {
            db::rename(&conn, name, new_name)?;
            println!("renamed {name} -> {new_name}");
        }

        Cmd::Remove { name } => {
            db::delete(&conn, name)?;
            println!("removed {name}");
        }

        Cmd::Todo { cmd } => match cmd {
            TodoCmd::List { plan, status, all } => {
                if let Some(s) = status.as_deref()
                    && !db::TODO_STATUSES.contains(&s)
                {
                    anyhow::bail!("unknown todo status '{s}': use pending, in_progress, or done");
                }
                if cli.json {
                    if db::get(&conn, plan)?.is_none() {
                        anyhow::bail!("unknown plan '{plan}'");
                    }
                    let mut todos = db::todos_of(&conn, plan)?;
                    let visible = tools::visible_sections(status.as_deref(), *all);
                    todos.retain(|t| visible.contains(&t.status.as_str()));
                    println!("{}", serde_json::to_string_pretty(&todos)?);
                } else {
                    println!(
                        "{}",
                        tools::todo_list_impl(&project, plan, status.clone(), *all)?
                    );
                }
            }
            TodoCmd::Add { plan, text } => {
                let id = db::todo_add(&conn, plan, text)?;
                println!("todo {id} added to {plan}");
            }
            TodoCmd::Edit {
                id,
                text,
                status,
                order,
            } => {
                db::todo_edit(
                    &conn,
                    *id,
                    &db::TodoPatch {
                        text: text.as_deref(),
                        status: status.as_deref(),
                        order: *order,
                    },
                )?;
                println!("todo {id} updated");
            }
            TodoCmd::Remove { id } => {
                db::todo_remove(&conn, *id)?;
                println!("todo {id} removed");
            }
        },

        Cmd::Note { plan, text } => {
            let id = db::note_add(&conn, plan, text)?;
            println!("note {id} added to {plan}");
        }

        Cmd::Adopt => unreachable!("handled before opening the project"),
    }

    Ok(())
}
