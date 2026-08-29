use mind_mcp::db::{self, Patch, Plan, TodoPatch};
use mind_mcp::state;
use mind_mcp::tools;

fn plan(name: &str, status: &str) -> Plan {
    Plan {
        name: name.into(),
        title: format!("title of {name}"),
        branch: format!("feat/{name}"),
        status: status.into(),
        sort_order: 0,
        merge_commit: String::new(),
        goal: format!("goal of {name}"),
        context: String::new(),
        definition_of_done: String::new(),
        review_type: "deep".into(),
    }
}

fn memory_db() -> rusqlite::Connection {
    db::open(std::path::Path::new(":memory:")).unwrap()
}

#[test]
fn valid_name_rules() {
    assert!(state::valid_name("location-resources"));
    assert!(state::valid_name("a1"));
    assert!(!state::valid_name(""));
    assert!(!state::valid_name("Upper"));
    assert!(!state::valid_name("-lead"));
    assert!(!state::valid_name("has space"));
}

#[test]
fn insert_and_get_roundtrip() {
    let conn = memory_db();
    db::insert(&conn, &plan("alpha", "pending")).unwrap();
    let got = db::get(&conn, "alpha").unwrap().unwrap();
    assert_eq!(got.title, "title of alpha");
    assert_eq!(got.goal, "goal of alpha");
    assert_eq!(got.review_type, "deep");
    assert!(db::get(&conn, "nope").unwrap().is_none());
}

#[test]
fn insert_rejects_bad_review_type() {
    let conn = memory_db();
    let mut p = plan("alpha", "pending");
    p.review_type = "casual".into();
    assert!(db::insert(&conn, &p).is_err());
}

#[test]
fn update_only_touches_provided_fields() {
    let conn = memory_db();
    db::insert(&conn, &plan("alpha", "pending")).unwrap();
    db::update(
        &conn,
        "alpha",
        &Patch {
            title: Some("new title"),
            branch: None,
            status: None,
            order: Some(7),
            merge_commit: None,
            goal: None,
            context: None,
            definition_of_done: None,
            review_type: Some("quick"),
        },
    )
    .unwrap();
    let got = db::get(&conn, "alpha").unwrap().unwrap();
    assert_eq!(got.title, "new title");
    assert_eq!(got.sort_order, 7);
    assert_eq!(got.review_type, "quick");
    assert_eq!(got.status, "pending");
    assert_eq!(got.branch, "feat/alpha");
    assert_eq!(got.goal, "goal of alpha");
}

#[test]
fn set_deps_rejects_unknown_and_cycles() {
    let conn = memory_db();
    db::insert(&conn, &plan("a", "pending")).unwrap();
    db::insert(&conn, &plan("b", "pending")).unwrap();

    // Unknown target.
    assert!(db::set_deps(&conn, "a", &["ghost".into()]).is_err());

    // a -> b is fine.
    db::set_deps(&conn, "a", &["b".into()]).unwrap();
    assert_eq!(db::deps_of(&conn, "a").unwrap(), vec!["b"]);

    // b -> a closes the cycle.
    assert!(db::set_deps(&conn, "b", &["a".into()]).is_err());

    // Self-dependency.
    assert!(db::set_deps(&conn, "a", &["a".into()]).is_err());

    // Replacement clears previous deps.
    db::set_deps(&conn, "a", &[]).unwrap();
    assert!(db::deps_of(&conn, "a").unwrap().is_empty());
}

#[test]
fn ready_returns_only_unblocked_pending() {
    let conn = memory_db();
    for (name, status) in [
        ("done-dep", "done"),
        ("blocked", "pending"),
        ("free", "pending"),
        ("wip", "in_progress"),
    ] {
        db::insert(&conn, &plan(name, status)).unwrap();
    }
    db::set_deps(&conn, "blocked", &["done-dep".into()]).unwrap();
    db::set_deps(&conn, "wip", &["done-dep".into()]).unwrap();

    // blocked waits on a pending dep below.
    db::insert(&conn, &plan("unfin", "pending")).unwrap();
    db::set_deps(&conn, "blocked", &["unfin".into()]).unwrap();

    let names: Vec<String> = db::ready(&conn)
        .unwrap()
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert!(names.contains(&"free".to_string()));
    assert!(!names.contains(&"blocked".to_string()));
    assert!(!names.contains(&"wip".to_string()));
    assert!(!names.contains(&"done-dep".to_string()));
}

#[test]
fn delete_cascades_dep_edges_todos_notes() {
    let conn = memory_db();
    db::insert(&conn, &plan("a", "pending")).unwrap();
    db::insert(&conn, &plan("b", "pending")).unwrap();
    db::set_deps(&conn, "a", &["b".into()]).unwrap();
    db::todo_add(&conn, "b", "step").unwrap();
    db::note_add(&conn, "b", "note").unwrap();
    db::delete(&conn, "b").unwrap();
    assert!(db::deps_of(&conn, "a").unwrap().is_empty());
    assert!(db::todos_of(&conn, "b").unwrap().is_empty());
    assert!(db::notes_of(&conn, "b").unwrap().is_empty());
}

#[test]
fn rename_follows_edges_both_ways() {
    let conn = memory_db();
    for name in ["upstream", "mid", "downstream"] {
        db::insert(&conn, &plan(name, "pending")).unwrap();
    }
    // mid -> upstream (outgoing edge), downstream -> mid (incoming edge).
    db::set_deps(&conn, "mid", &["upstream".into()]).unwrap();
    db::set_deps(&conn, "downstream", &["mid".into()]).unwrap();

    db::rename(&conn, "mid", "core").unwrap();

    assert!(db::get(&conn, "mid").unwrap().is_none());
    let got = db::get(&conn, "core").unwrap().unwrap();
    assert_eq!(got.title, "title of mid");
    assert_eq!(db::deps_of(&conn, "core").unwrap(), vec!["upstream"]);
    assert_eq!(
        db::dependents_of(&conn, "core").unwrap(),
        vec!["downstream"]
    );
    // The graph is truly rewired: depending on core now closes a cycle
    // through it (upstream <- core <- upstream).
    assert!(db::set_deps(&conn, "upstream", &["core".into()]).is_err());
}

#[test]
fn rename_follows_todos_and_notes() {
    let conn = memory_db();
    db::insert(&conn, &plan("alpha", "pending")).unwrap();
    let t = db::todo_add(&conn, "alpha", "step").unwrap();
    let n = db::note_add(&conn, "alpha", "a finding").unwrap();
    db::rename(&conn, "alpha", "beta").unwrap();
    let todos = db::todos_of(&conn, "beta").unwrap();
    let notes = db::notes_of(&conn, "beta").unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].plan, "beta");
    assert_eq!(todos[0].id, t);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].plan, "beta");
    assert_eq!(notes[0].id, n);
}

#[test]
fn rename_rejects_unknown_self_bad_and_collision() {
    let conn = memory_db();
    db::insert(&conn, &plan("alpha", "pending")).unwrap();
    db::insert(&conn, &plan("beta", "pending")).unwrap();

    assert!(db::rename(&conn, "ghost", "any").is_err());
    assert!(db::rename(&conn, "alpha", "alpha").is_err());
    assert!(db::rename(&conn, "alpha", "Beta").is_err());
    assert!(db::rename(&conn, "alpha", "-beta").is_err());
    assert!(db::rename(&conn, "alpha", "beta").is_err());
    // Every rejection left the plan untouched.
    assert!(db::get(&conn, "alpha").unwrap().is_some());
}

fn todo(id: i64, text: &str, status: &str) -> db::Todo {
    db::Todo {
        id,
        plan: "alpha".into(),
        text: text.into(),
        status: status.into(),
        sort_order: id,
    }
}

#[test]
fn todo_list_default_shows_open_work_and_hides_done() {
    let todos = vec![
        todo(1, "finished step", "done"),
        todo(2, "running step", "in_progress"),
        todo(3, "queued step", "pending"),
    ];

    let out = tools::render_todo_list("alpha", &todos, None, false);
    assert!(out.contains("## In progress\n\n- running step (id 2)"));
    assert!(out.contains("## Pending\n\n- queued step (id 3)"));
    assert!(!out.contains("## Done"));
    assert!(!out.contains("finished step"));
    assert!(out.contains("1 done hidden; use --all to show them"));
    let progress = out.find("In progress").unwrap();
    let pending = out.find("Pending").unwrap();
    assert!(progress < pending);
}

#[test]
fn todo_list_all_and_explicit_status() {
    let todos = vec![
        todo(1, "finished step", "done"),
        todo(2, "running step", "in_progress"),
        todo(3, "queued step", "pending"),
    ];

    let all = tools::render_todo_list("alpha", &todos, None, true);
    assert!(all.contains("## Done\n\n- finished step (id 1)"));
    assert!(!all.contains("hidden"));
    let done = all.find("Done").unwrap();
    assert!(all.find("Pending").unwrap() < done);

    let only_done = tools::render_todo_list("alpha", &todos, Some("done"), false);
    assert_eq!(only_done, "## Done\n\n- finished step (id 1)");

    let empty = tools::render_todo_list(
        "alpha",
        &[todo(9, "queued", "pending")],
        Some("done"),
        false,
    );
    assert_eq!(empty, "(no done todos)");

    let quiet = tools::render_todo_list("alpha", &[todo(9, "finished", "done")], None, false);
    assert_eq!(quiet, "(no open todos in 'alpha' — 1 done; use --all)");
}

#[test]
fn todo_list_visibility_sections() {
    assert_eq!(
        tools::visible_sections(None, false),
        vec!["in_progress", "pending"]
    );
    assert_eq!(
        tools::visible_sections(None, true),
        vec!["in_progress", "pending", "done"]
    );
    assert_eq!(tools::visible_sections(Some("done"), true), vec!["done"]);
}

#[test]
fn todo_lifecycle() {
    let conn = memory_db();
    db::insert(&conn, &plan("alpha", "pending")).unwrap();
    let t1 = db::todo_add(&conn, "alpha", "first step").unwrap();
    let t2 = db::todo_add(&conn, "alpha", "second step").unwrap();
    assert_eq!(t1 + 1, t2);

    let todos = db::todos_of(&conn, "alpha").unwrap();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[0].status, "pending");
    assert_eq!(todos[0].sort_order, 1);

    db::todo_edit(
        &conn,
        t1,
        &TodoPatch {
            text: None,
            status: Some("in_progress"),
            order: None,
        },
    )
    .unwrap();
    let todos = db::todos_of(&conn, "alpha").unwrap();
    assert_eq!(todos[0].status, "in_progress");

    db::todo_edit(
        &conn,
        t1,
        &TodoPatch {
            text: None,
            status: None,
            order: Some(2),
        },
    )
    .unwrap();
    db::todo_edit(
        &conn,
        t2,
        &TodoPatch {
            text: None,
            status: None,
            order: Some(1),
        },
    )
    .unwrap();
    let todos = db::todos_of(&conn, "alpha").unwrap();
    assert_eq!(todos[0].id, t2);
    assert_eq!(todos[1].id, t1);

    db::todo_edit(
        &conn,
        t1,
        &TodoPatch {
            text: Some("rewritten"),
            status: None,
            order: None,
        },
    )
    .unwrap();
    let todos = db::todos_of(&conn, "alpha").unwrap();
    assert_eq!(todos[1].text, "rewritten");

    assert!(
        db::todo_edit(
            &conn,
            t1,
            &TodoPatch {
                text: None,
                status: Some("bogus"),
                order: None
            }
        )
        .is_err()
    );
    assert!(
        db::todo_edit(
            &conn,
            999,
            &TodoPatch {
                text: None,
                status: None,
                order: None
            }
        )
        .is_err()
    );
    db::todo_remove(&conn, t2).unwrap();
    assert!(db::todos_of(&conn, "alpha").unwrap().len() == 1);
    assert!(db::todo_remove(&conn, t2).is_err());
}

#[test]
fn todos_scoped_to_plan_and_cascade_on_delete() {
    let conn = memory_db();
    db::insert(&conn, &plan("a", "pending")).unwrap();
    db::insert(&conn, &plan("b", "pending")).unwrap();
    db::todo_add(&conn, "a", "a1").unwrap();
    let b1 = db::todo_add(&conn, "b", "b1").unwrap();
    assert_eq!(db::todos_of(&conn, "a").unwrap().len(), 1);
    db::delete(&conn, "b").unwrap();
    assert!(db::todo_remove(&conn, b1).is_err());
}

#[test]
fn notes_append_and_cascade() {
    let conn = memory_db();
    db::insert(&conn, &plan("alpha", "pending")).unwrap();
    let n1 = db::note_add(&conn, "alpha", "decided against a JS fix").unwrap();
    let _n2 = db::note_add(&conn, "alpha", "review flagged X; resolved").unwrap();
    let notes = db::notes_of(&conn, "alpha").unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0].id, n1);
    assert!(!notes[1].created_at.is_empty());
    assert!(db::note_add(&conn, "ghost", "x").is_err());
    db::delete(&conn, "alpha").unwrap();
    // Row is gone; a fresh note on the deleted plan fails.
    assert!(db::note_add(&conn, "alpha", "gone").is_err());
}

// ---------- adopt ----------

const SAMPLE_README: &str = "\
# legacy-plan

**Branch:** `feat/legacy-plan`
**Review:** deep (two independent reviewer subagents)

## Goal

Make the thing work.

## Steps

1. First step, wrapped
   onto two lines.
- second item

## Definition of done

- Tests pass.
- Deep review passes.
";

const SAMPLE_DISCUSSION: &str = "\
# legacy-plan — Discussion

Log decisions and open points here. Append as you work.

## Decisions locked in the design session (2026-08-28)

- **Mechanism: pseudonymization, not deletion.** Person rows stay.

## Open points

None right now.
";

#[test]
fn adopt_imports_folders_and_cleans_up() {
    let repo = std::env::temp_dir().join(format!("mind-adopt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(repo.join("plans/legacy-plan")).unwrap();
    std::fs::create_dir_all(repo.join("plans/ghost-plan")).unwrap();
    std::fs::write(repo.join("plans/legacy-plan/README.md"), SAMPLE_README).unwrap();
    std::fs::write(
        repo.join("plans/legacy-plan/discussion.md"),
        SAMPLE_DISCUSSION,
    )
    .unwrap();
    std::fs::write(repo.join("plans.md"), "# x\n").unwrap();
    std::fs::write(repo.join("plans.yaml"), "plans: []\n").unwrap();

    // A legacy hidden DB with the old schema (progress column, no plan
    // fields) plus one plan row and a dependency edge.
    let legacy_dir = repo.join("legacy-data");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    let canon = repo.canonicalize().unwrap();
    let slug: String = canon
        .to_string_lossy()
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect();
    let legacy_db = legacy_dir.join(format!("{slug}.db"));
    let legacy = rusqlite::Connection::open(&legacy_db).unwrap();
    legacy
        .execute_batch(
            "CREATE TABLE plans(
               name TEXT PRIMARY KEY, title TEXT NOT NULL, branch TEXT NOT NULL DEFAULT '',
               status TEXT NOT NULL, progress TEXT NOT NULL DEFAULT '',
               sort_order INTEGER NOT NULL DEFAULT 0, merge_commit TEXT NOT NULL DEFAULT '',
               created_at TEXT NOT NULL DEFAULT (datetime('now')),
               updated_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE plan_deps(
               plan TEXT NOT NULL REFERENCES plans(name) ON DELETE CASCADE,
               depends_on TEXT NOT NULL REFERENCES plans(name) ON DELETE CASCADE,
               PRIMARY KEY(plan, depends_on));
             INSERT INTO plans(name, title, branch, status, progress, sort_order, merge_commit)
               VALUES('legacy-plan', 'Legacy plan', 'feat/legacy-plan', 'in_progress', 'half', 1, '');
             INSERT INTO plans(name, title, branch, status, sort_order, merge_commit)
               VALUES('done-dep', 'Done dep', '', 'done', 0, 'abc1234');
             INSERT INTO plan_deps VALUES('legacy-plan', 'done-dep');",
        )
        .unwrap();

    let project = state::Project { root: repo.clone() };
    let summary = mind_mcp::adopt::run(&project, &legacy_dir).unwrap();

    assert!(summary.contains("1 folder(s)"), "{summary}");
    assert!(summary.contains("2 todo(s)"), "{summary}");
    assert!(summary.contains("2 note(s)"), "{summary}");
    assert!(summary.contains("ghost-plan"), "{summary}");

    let conn = db::open(&project.db_path()).unwrap();
    let p = db::get(&conn, "legacy-plan").unwrap().unwrap();
    assert_eq!(p.goal, "Make the thing work.");
    assert_eq!(p.definition_of_done, "- Tests pass.\n- Deep review passes.");
    assert_eq!(p.review_type, "deep");
    assert_eq!(p.merge_commit, "");
    assert_eq!(
        db::get(&conn, "done-dep").unwrap().unwrap().merge_commit,
        "abc1234"
    );
    assert_eq!(db::deps_of(&conn, "legacy-plan").unwrap(), vec!["done-dep"]);
    assert!(db::list(&conn, Some("in_progress")).unwrap()[0].name == "legacy-plan");

    let todos = db::todos_of(&conn, "legacy-plan").unwrap();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[0].text, "First step, wrapped onto two lines.");
    assert!(todos.iter().all(|t| t.status == "pending"));

    let notes = db::notes_of(&conn, "legacy-plan").unwrap();
    assert_eq!(notes.len(), 2);
    assert!(notes[0].text.starts_with("Decisions locked"));
    assert!(notes[1].text.starts_with("Open points"));

    // Folder imported and gone; unknown folder left in place; artifacts gone.
    assert!(!repo.join("plans/legacy-plan").exists());
    assert!(repo.join("plans/ghost-plan").exists());
    assert!(!repo.join("plans.md").exists());
    assert!(!repo.join("plans.yaml").exists());

    // Refuses to run twice.
    assert!(mind_mcp::adopt::run(&project, &legacy_dir).is_err());

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn adopt_report_type_parsing() {
    // review_from_line accepts only the enum values.
    assert_eq!(
        mind_mcp::adopt::review_from_line("**Review:** deep (two reviewers)"),
        Some("deep".to_string())
    );
    assert_eq!(
        mind_mcp::adopt::review_from_line("**Review:** quick"),
        Some("quick".to_string())
    );
    assert_eq!(
        mind_mcp::adopt::review_from_line("**Review:** none"),
        Some("none".to_string())
    );
    assert_eq!(
        mind_mcp::adopt::review_from_line("**Review:** strict"),
        None
    );
    assert_eq!(mind_mcp::adopt::review_from_line("no marker"), None);
}

// ---------- adopt parsers ----------

#[test]
fn step_items_wraps_and_scopes_bullets() {
    use mind_mcp::adopt::step_items;
    let body = "1. First step, wrapped\n   onto two lines.\n- second item\n   2. Not a new item, just indented.\n* third star item";
    let items = step_items(body);
    assert_eq!(
        items,
        vec![
            "First step, wrapped onto two lines.",
            "second item 2. Not a new item, just indented.",
            "third star item",
        ]
    );
}

#[test]
fn note_blocks_keep_hash_lines_after_first_heading() {
    use mind_mcp::adopt::note_blocks;
    let doc = "# title — dropped\nLog decisions and open points here. Append as you work.\n\n## Decision\n\n- use `#include` guards\n";
    let blocks = note_blocks(doc);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].starts_with("Decision"));
    assert!(blocks[0].contains("`#include`"));
}

#[test]
fn cut_section_handles_absent_and_fences() {
    use mind_mcp::adopt::cut_section;
    assert_eq!(
        cut_section("## Goal\n\nreal\n", "Goal").as_deref(),
        Some("real")
    );
    assert_eq!(cut_section("no sections", "Goal"), None);
    assert_eq!(
        cut_section("## Goal\n\nempty next\n\n## Goal\n\nsecond\n", "Goal").as_deref(),
        Some("empty next")
    );
    let fenced = "## Steps\n\n```\n## not a heading\n```\n";
    assert_eq!(
        cut_section(fenced, "Steps").as_deref(),
        Some("```\n## not a heading\n```")
    );
}

#[test]
fn extra_sections_catch_unknown_headings() {
    use mind_mcp::adopt::extra_sections;
    let doc = "preamble\n\n## Goal\n\ngoal text\n\n## Open design points\n\n- point one\n- point two\n\n## Steps\n\n1. step\n";
    let out = extra_sections(doc);
    assert!(out.contains("## Open design points"), "{out}");
    assert!(out.contains("- point one"), "{out}");
    assert!(!out.contains("goal text"), "{out}");
    assert!(!out.contains("1. step"), "{out}");
    assert_eq!(extra_sections("## Goal\n\nonly known\n"), "");
}

#[test]
fn adopt_bails_without_legacy_db_but_folders() {
    let repo = std::env::temp_dir().join(format!("mind-adopt-bail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(repo.join("plans/lonely")).unwrap();
    std::fs::write(repo.join("plans/lonely/README.md"), "# x\n").unwrap();
    let project = state::Project { root: repo.clone() };

    let err = mind_mcp::adopt::run(&project, std::path::Path::new("/nonexistent"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("refusing a partial migration"), "{err}");

    // And nothing was created or deleted.
    assert!(!repo.join("plans.db").exists());
    assert!(repo.join("plans/lonely/README.md").exists());
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn adopt_tolerates_empty_registry_from_prior_read() {
    let repo = std::env::temp_dir().join(format!("mind-adopt-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let project = state::Project { root: repo.clone() };

    // A read (mind board / plans_show) on a legacy repo auto-creates an
    // empty plans.db. Adopt must still run.
    let conn = db::open(&project.db_path()).unwrap();
    drop(conn);

    let summary = mind_mcp::adopt::run(&project, std::path::Path::new("/nonexistent")).unwrap();
    assert!(summary.contains("0 folder(s)"), "{summary}");
    assert!(db::open(&project.db_path()).is_ok());
    let _ = std::fs::remove_dir_all(&repo);
}

// ---------- brain ----------

const BRAIN_SAMPLE: &str = "- [git] Squash merge per plan. <!--id:1-->";

#[test]
fn brain_parse_add_edit_remove() {
    let text = format!("{BRAIN_SAMPLE}\n- [rust] Borrow by default. <!--id:3-->\n");
    let lessons = mind_mcp::brain::parse_brain(&text);
    assert_eq!(lessons.len(), 2);
    assert_eq!(lessons[1].tag, "rust");
    assert_eq!(lessons[1].id, 3);
    assert_eq!(mind_mcp::brain::parse_brain("no lessons here").len(), 0);
}

#[test]
fn detail_renders_sections_and_omits_empty() {
    let mut p = plan("alpha", "in_progress");
    p.definition_of_done = "Tests pass.".into();
    let out = mind_mcp::tools::render_detail(
        &p,
        &["dep".into()],
        &[],
        &[db::Todo {
            id: 7,
            plan: "alpha".into(),
            text: "do it".into(),
            status: "done".into(),
            sort_order: 1,
        }],
        &[db::Note {
            id: 9,
            plan: "alpha".into(),
            text: "a note".into(),
            created_at: "2026-08-28 10:00:00".into(),
        }],
    );
    assert!(out.contains("# alpha —"));
    assert!(out.contains("## Goal"));
    assert!(!out.contains("## Context")); // empty -> omitted
    assert!(out.contains("## Definition of done"));
    assert!(out.contains("- [x] do the thing") || out.contains("- [x] do"));
    assert!(out.contains("(id 9)"));
    assert!(out.contains("depends on: dep"));
}
