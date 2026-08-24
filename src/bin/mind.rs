//! Human-facing CLI over the mind registry: browse plans, the dependency
//! tree, and mutate entries exactly like the MCP tools do — same database,
//! same file sync, no drift.

use anyhow::Context;
use clap::{Parser, Subcommand};
use mind_mcp::{db, markdown, state::Project};

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
    /// One plan in full: progress, branch, merge commit, dependents.
    Show { name: String },
    /// Pending plans whose every dependency is done.
    Ready,
    Add {
        name: String,
        title: String,
        #[arg(long, default_value = "pending")]
        status: String,
        #[arg(long)]
        branch: Option<String>,
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
        progress: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        merge_commit: Option<String>,
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
    },
    /// Renames a plan; every dependency edge pointing at it follows.
    /// Rename a plans/<name>/ folder yourself — the registry tracks names,
    /// not folders.
    Rename { name: String, new_name: String },
    /// Deletes a plan and its dependency links.
    Remove { name: String },
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
    let path = project.db_path()?;
    let conn = db::open(&path)
        .with_context(|| format!("no plan registry for {} ({path:?})", project.root.display()))?;
    Ok((project, conn))
}

/// Mutations mirror the MCP server's post-write behaviour: regenerate the
/// repo snapshots so human edits never drift from agent edits.
fn sync_files(project: &Project) -> anyhow::Result<()> {
    markdown::sync_plans_md(project)?;
    markdown::export_yaml(project)?;
    Ok(())
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
            let plan = db::get(&conn, name)?.context("unknown plan")?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
                return Ok(());
            }
            println!("name:         {}", plan.name);
            println!("title:        {}", plan.title);
            println!("status:       {} {}", glyphs(&plan.status), plan.status);
            println!("branch:       {}", plan.branch);
            println!("merge commit: {}", plan.merge_commit);
            println!("depends on:   {}", db::deps_of(&conn, name)?.join(", "));
            println!(
                "dependents:   {}",
                db::dependents_of(&conn, name)?.join(", ")
            );
            if !plan.progress.is_empty() {
                println!("progress:\n  {}", plan.progress.replace('\n', "\n  "));
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
            status,
            branch,
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
            let order = match after {
                Some(prev) => prev + 1,
                None => db::next_order(&conn)?,
            };
            let plan = db::Plan {
                name: name.clone(),
                title: title.clone(),
                branch: branch.clone().unwrap_or_default(),
                status: status.clone(),
                progress: String::new(),
                sort_order: order,
                merge_commit: String::new(),
            };
            db::insert(&conn, &plan)?;
            if !depends_on.is_empty() {
                db::set_deps(&conn, name, depends_on)?;
            }
            sync_files(&project)?;
            println!("added {name}");
        }

        Cmd::Update {
            name,
            status,
            progress,
            branch,
            merge_commit,
            depends_on,
        } => {
            let patch = db::Patch {
                title: None,
                branch: branch.as_deref(),
                status: status.as_deref(),
                progress: progress.as_deref(),
                order: None,
                merge_commit: merge_commit.as_deref(),
            };
            db::update(&conn, name, &patch)?;
            if !depends_on.is_empty() {
                db::set_deps(&conn, name, depends_on)?;
            }
            sync_files(&project)?;
            println!("updated {name}");
        }

        Cmd::Rename { name, new_name } => {
            db::rename(&conn, name, new_name)?;
            sync_files(&project)?;
            println!("renamed {name} -> {new_name}");
        }

        Cmd::Remove { name } => {
            db::delete(&conn, name)?;
            sync_files(&project)?;
            println!("removed {name}");
        }
    }

    Ok(())
}
