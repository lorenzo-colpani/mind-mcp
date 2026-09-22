//! Resolver and registry home: slug derivation, legacy migration, fork
//! refusal, markers, and the export/import round trip against the
//! state dir.

mod common;

use common::{git_repo, plan_row, project_for, state_home, tmp_dir};
use mind_mcp::state;
use mind_mcp::{db, snapshot};

// ---------- slug derivation ----------

#[test]
fn slug_from_url_covers_the_git_forms() {
    let cases = [
        (
            "git@github.com:lorenzo-colpani/bebaiha.git",
            "lorenzo-colpani-bebaiha",
        ),
        ("https://github.com/Owner/Repo.git", "owner-repo"),
        ("https://user:pass@github.com/owner/repo.git", "owner-repo"),
        ("ssh://git@host.example:2222/owner/repo.git", "owner-repo"),
        ("https://github.com/owner/repo", "owner-repo"),
        ("git@github.com:owner/repo_name.git", "owner-repo_name"),
        // Single-segment path: the repo name alone.
        ("https://host.example/repo.git", "repo"),
        // Local path origin: the last two segments.
        ("/home/dev/work/repo", "work-repo"),
    ];
    for (url, want) in cases {
        assert_eq!(state::slug_from_url(url).as_deref(), Some(want), "{url}");
    }
}

#[test]
fn slug_from_url_rejects_unusable_urls() {
    for url in ["", "  ", "git@host:", "://"] {
        assert_eq!(state::slug_from_url(url), None, "{url}");
    }
}

#[test]
fn slug_prefers_origin_remote_over_basename() {
    let dir = git_repo("origin-slug", "git@github.com:acme/widgets.git");
    assert_eq!(state::slug_for_root(&dir).unwrap(), "acme-widgets");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn slug_falls_back_to_root_basename_without_origin() {
    let dir = std::env::temp_dir().join(format!("My Repo_2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let slug = state::slug_for_root(&dir).unwrap();
    // Spaces and casing sanitize away; the pid suffix stays.
    assert!(slug.starts_with("my-repo_2-"), "{slug}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn long_slugs_truncate_with_a_unique_suffix() {
    let repo = "a".repeat(250);
    let dir = git_repo("long-slug", &format!("git@github.com:acme/{repo}.git"));
    let slug = state::slug_for_root(&dir).unwrap();
    assert_eq!(slug.len(), state::max_slug_len());
    assert!(
        slug.starts_with("acme-") && slug.contains(&"a".repeat(150)),
        "{slug}"
    );
    // Deterministic across calls.
    assert_eq!(state::slug_for_root(&dir).unwrap(), slug);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn origin_slug_is_stable_across_a_move() {
    let home = state_home("move");
    let dir = git_repo("move-slug", "git@github.com:acme/widgets.git");
    let before = project_for(&dir, &home).db_path();
    let moved = dir.parent().unwrap().join("widgets-relocated");
    std::fs::rename(&dir, &moved).unwrap();
    let after = project_for(&moved, &home).db_path();
    assert_eq!(before, after);
    let _ = std::fs::remove_dir_all(&moved);
    let _ = std::fs::remove_dir_all(&home);
}

// ---------- migration and fork refusal ----------

#[test]
fn fresh_checkout_creates_an_empty_registry() {
    let home = state_home("fresh");
    let repo = tmp_dir("fresh-repo");
    let project = project_for(&repo, &home);
    let conn = project.open_registry().unwrap();
    assert!(db::list(&conn, None).unwrap().is_empty());
    assert!(project.db_path().exists());
    assert!(project.lock_path().exists());
    assert!(project.state_dir().join("repo-id").exists());
    assert!(!repo.join("plans.db").exists());
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn legacy_repo_root_registry_moves_into_the_state_dir() {
    let home = state_home("legacy");
    let repo = tmp_dir("legacy-repo");
    // Seed the legacy registry with one plan.
    let legacy = db::open(&repo.join("plans.db")).unwrap();
    db::insert(&legacy, &plan_row("kept")).unwrap();
    drop(legacy);

    let project = project_for(&repo, &home);
    let conn = project.open_registry().unwrap();
    assert!(db::get(&conn, "kept").unwrap().is_some());
    assert!(!repo.join("plans.db").exists());
    assert!(project.db_path().exists());
    assert!(project.lock_path().exists());
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn both_registries_refuse_in_words() {
    let home = state_home("forked");
    let repo = tmp_dir("forked-repo");
    let project = project_for(&repo, &home);
    // Open once: creates the state-dir registry.
    drop(project.open_registry().unwrap());
    // A stray legacy file reappears at the repo root.
    std::fs::File::create(repo.join("plans.db")).unwrap();

    let err = project.open_registry().unwrap_err().to_string();
    assert!(err.contains("two registries"), "{err}");
    assert!(
        err.contains(project.db_path().display().to_string().as_str()),
        "{err}"
    );
    assert!(
        err.contains(repo.join("plans.db").display().to_string().as_str()),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn prepare_is_idempotent() {
    let home = state_home("repeat");
    let repo = tmp_dir("repeat-repo");
    let project = project_for(&repo, &home);
    for _ in 0..3 {
        project.prepare().unwrap();
    }
    assert!(project.lock_path().exists());
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&home);
}

// ---------- one registry per repo: markers ----------

#[test]
fn same_slug_dir_refuses_a_different_repo() {
    let home = state_home("clash");
    // Two unrelated local-only repos with the same basename: same slug,
    // different checkout roots.
    let root = tmp_dir("clash-root");
    let alice = root.join("a").join("shared-name");
    let bob = root.join("b").join("shared-name");
    std::fs::create_dir_all(&alice).unwrap();
    std::fs::create_dir_all(&bob).unwrap();

    drop(project_for(&alice, &home).open_registry().unwrap());
    let err = project_for(&bob, &home)
        .open_registry()
        .unwrap_err()
        .to_string();
    assert!(err.contains("hosts another repo"), "{err}");
    let _ = std::fs::remove_dir_all(&alice);
    let _ = std::fs::remove_dir_all(&bob);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn same_repo_across_checkouts_shares_the_registry() {
    let home = state_home("share");
    // Two clones of one origin: same slug, same identity, one registry.
    let clone_a = git_repo("share-a", "git@github.com:acme/shared.git");
    let clone_b = git_repo("share-b", "git@github.com:acme/shared.git");
    let a = project_for(&clone_a, &home);
    let b = project_for(&clone_b, &home);
    assert_eq!(a.db_path(), b.db_path());
    let conn = a.open_registry().unwrap();
    db::insert(&conn, &plan_row("visible")).unwrap();
    drop(conn);
    let conn = b.open_registry().unwrap();
    assert!(db::get(&conn, "visible").unwrap().is_some());
    let _ = std::fs::remove_dir_all(&clone_a);
    let _ = std::fs::remove_dir_all(&clone_b);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn adding_origin_later_refuses_to_fork() {
    let home = state_home("late-origin");
    // Local-only repo: registry under the basename slug, marker holds
    // the checkout root.
    let repo = git_repo("late-origin", "");
    let project = project_for(&repo, &home);
    let conn = project.open_registry().unwrap();
    db::insert(&conn, &plan_row("history")).unwrap();
    drop(conn);
    assert!(project.slug.starts_with("mind-late-origin-"), "{project:?}");

    // The repo gains an origin: the slug changes. A fresh dir would
    // fork the history, so the resolver refuses.
    let ok = std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github.com:acme/late-origin.git",
        ])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let renamed = project_for(&repo, &home);
    assert_eq!(renamed.slug, "acme-late-origin");
    let err = renamed.open_registry().unwrap_err().to_string();
    assert!(err.contains("already has a registry"), "{err}");
    // The old registry still opens under its original identity.
    assert!(
        db::get(&project.open_registry().unwrap(), "history")
            .unwrap()
            .is_some()
    );
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&home);
}

// ---------- portability against the new home ----------

#[test]
fn export_import_roundtrip_across_two_repos() {
    let home = state_home("roundtrip");
    let source_repo = tmp_dir("roundtrip-source");
    let target_repo = tmp_dir("roundtrip-target");

    let source = project_for(&source_repo, &home);
    let conn = source.open_registry().unwrap();
    db::insert(&conn, &plan_row("ported")).unwrap();
    db::todo_add(&conn, "ported", "first step").unwrap();
    db::note_add(&conn, "ported", "a decision").unwrap();

    let snap_path = source_repo.join("roundtrip-snap.yaml");
    let snap = snapshot::export(&conn).unwrap();
    snapshot::write(snap_path.to_str().unwrap(), &snap).unwrap();
    drop(conn);

    // The target repo resolves to its own registry dir; the snapshot
    // ports the state across.
    let target = project_for(&target_repo, &home);
    let conn = target.open_registry().unwrap();
    let read = snapshot::read(snap_path.to_str().unwrap()).unwrap();
    snapshot::import(&conn, &read, false).unwrap();

    let ported = db::get(&conn, "ported").unwrap().unwrap();
    assert_eq!(ported.title, "title of ported");
    assert_eq!(db::todos_of(&conn, "ported").unwrap().len(), 1);
    assert_eq!(db::notes_of(&conn, "ported").unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(&source_repo);
    let _ = std::fs::remove_dir_all(&target_repo);
    let _ = std::fs::remove_dir_all(&home);
}
