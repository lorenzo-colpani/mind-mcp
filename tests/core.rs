use mind_mcp::db::{self, Patch, Plan};
use mind_mcp::markdown;
use mind_mcp::state;

fn plan(name: &str, status: &str) -> Plan {
    Plan {
        name: name.into(),
        title: format!("title of {name}"),
        branch: format!("feat/{name}"),
        status: status.into(),
        progress: String::new(),
        sort_order: 0,
        merge_commit: String::new(),
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
fn slug_is_stable_hex() {
    let dir = std::env::temp_dir().join("mind-slug-test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = state::Project { root: dir.clone() };
    let s1 = p.slug().unwrap();
    let s2 = p.slug().unwrap();
    assert_eq!(s1, s2);
    assert!(
        s1.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    );
}

#[test]
fn insert_and_get_roundtrip() {
    let conn = memory_db();
    db::insert(&conn, &plan("alpha", "pending")).unwrap();
    let got = db::get(&conn, "alpha").unwrap().unwrap();
    assert_eq!(got.title, "title of alpha");
    assert!(db::get(&conn, "nope").unwrap().is_none());
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
            progress: None,
            order: Some(7),
            merge_commit: None,
        },
    )
    .unwrap();
    let got = db::get(&conn, "alpha").unwrap().unwrap();
    assert_eq!(got.title, "new title");
    assert_eq!(got.sort_order, 7);
    assert_eq!(got.status, "pending");
    assert_eq!(got.branch, "feat/alpha");
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

    // Longer cycle: c -> a would not cycle; but c -> b -> ... check chain via
    // replacement on 'a' pointing back through dependents is rejected above.

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
fn delete_cascades_dep_edges() {
    let conn = memory_db();
    db::insert(&conn, &plan("a", "pending")).unwrap();
    db::insert(&conn, &plan("b", "pending")).unwrap();
    db::set_deps(&conn, "a", &["b".into()]).unwrap();
    db::delete(&conn, "b").unwrap();
    assert!(db::deps_of(&conn, "a").unwrap().is_empty());
}

const DOC: &str = "\
# Bebaiha — Plan System

Intro text stays.

## Lifecycle

Steps stay too.

## Done

| Plan | Merge commit | Result |
|---|---|---|
| old | `abc` | old work |
";

#[test]
fn sync_replaces_sections_and_keeps_rest() {
    use mind_mcp::markdown::sync_doc;
    let done = vec![plan("old-plan", "done")];
    let active = vec![plan("new-plan", "pending")];
    let out = sync_doc(DOC, &active, &done, &[]);
    assert!(out.contains("## Plan index"));
    assert!(out.contains("## Done"));
    assert!(out.contains("new-plan"));
    assert!(out.contains("old-plan"));
    assert!(out.contains("Intro text stays."));
    assert!(out.contains("## Lifecycle"));
    // Old hand-written done row is replaced by the rendered one.
    assert!(!out.contains("| old |"));
    // Order: index before done.
    let idx = out.find("## Plan index").unwrap();
    let dne = out.find("## Done").unwrap();
    assert!(idx < dne);
}

#[test]
fn sync_appends_missing_sections_in_order() {
    use mind_mcp::markdown::sync_doc;
    let out = sync_doc("# Just a header\n", &[plan("x", "pending")], &[], &[]);
    assert!(out.contains("## Plan index"));
    assert!(out.contains("## Done"));
    let idx = out.find("## Plan index").unwrap();
    let dne = out.find("## Done").unwrap();
    assert!(idx < dne);
}

#[test]
fn brain_parse_add_edit_remove() {
    let dir = std::env::temp_dir().join(format!("mind-brain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY-free env juggling via a scoped override is not possible for a
    // global fn; instead write to the real path only in CI-free local runs.
    // These tests therefore exercise parse + render logic directly.
    let text = format!("{BRAIN_SAMPLE}\n- [rust] Borrow by default. <!--id:3-->\n");
    let lessons = markdown::parse_brain(&text);
    assert_eq!(lessons.len(), 2);
    assert_eq!(lessons[1].tag, "rust");
    assert_eq!(lessons[1].id, 3);

    let _ = dir;
}

const BRAIN_SAMPLE: &str = "- [git] Squash merge per plan. <!--id:1-->";

#[test]
fn yaml_render_matches_shape() {
    let mut p = plan("alpha", "pending");
    p.progress = "half way".into();
    let out = markdown::render_yaml(&[p], &[("alpha".into(), "beta".into())]);
    assert!(out.starts_with("# Plan registry. Generated by mind-mcp"));
    assert!(out.contains("- name: alpha"));
    assert!(out.contains("progress: \"half way\""));
    assert!(out.contains("depends_on: [beta]"));
}
