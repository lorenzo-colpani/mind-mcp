//! Resolver and registry home: slug derivation, legacy migration, fork
//! refusal, and the export/import round trip against the state dir.

use mind_mcp::state::{self, Project};
use mind_mcp::{db, snapshot};

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mind-reg-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One isolated state home per test: parallel tests must never share,
/// or one test's cleanup deletes another test's open database.
fn state_home(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mind-reg-home-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A project whose registry lives in an isolated state home.
fn project_for(root: &std::path::Path, home: &std::path::Path) -> Project {
    Project {
        root: root.to_path_buf(),
        state_home: home.to_path_buf(),
        slug: state::slug_for_root(root).unwrap(),
    }
}

/// A real git repo with `url` as its origin remote.
fn git_repo_with_origin(tag: &str, url: &str) -> std::path::PathBuf {
    let dir = tmp_dir(tag);
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap()
        .success()
        .then_some(())
        .expect("git init");
    std::process::Command::new("git")
        .args(["remote", "add", "origin", url])
        .current_dir(&dir)
        .status()
        .unwrap()
        .success()
        .then_some(())
        .expect("git remote add");
    dir
}

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
    let dir = git_repo_with_origin("origin-slug", "git@github.com:acme/widgets.git");
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
fn origin_slug_is_stable_across_a_move() {
    let home = state_home("fresh");
    let dir = git_repo_with_origin("move-slug", "git@github.com:acme/widgets.git");
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
    let home = state_home("legacy");
    let repo = tmp_dir("fresh");
    let project = project_for(&repo, &home);
    let conn = project.open_registry().unwrap();
    assert!(db::list(&conn, None).unwrap().is_empty());
    assert!(project.db_path().exists());
    assert!(project.lock_path().exists());
    assert!(!repo.join("plans.db").exists());
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn legacy_repo_root_registry_moves_into_the_state_dir() {
    let home = state_home("forked");
    let repo = tmp_dir("legacy");
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
    let home = state_home("repeat");
    let repo = tmp_dir("forked");
    let project = project_for(&repo, &home);
    // Open once: creates the state-dir registry.
    drop(project.open_registry().unwrap());
    // A legacy file reappears at the repo root.
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
fn prepare_never_forks_on_repeat_calls() {
    let home = state_home("roundtrip");
    let repo = tmp_dir("repeat");
    let project = project_for(&repo, &home);
    for _ in 0..3 {
        project.prepare().unwrap();
    }
    assert!(project.lock_path().exists());
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&home);
}

// ---------- portability against the new home ----------

fn plan_row(name: &str) -> db::Plan {
    db::Plan {
        name: name.to_string(),
        title: format!("title of {name}"),
        branch: format!("feat/{name}"),
        status: "pending".into(),
        sort_order: 1,
        merge_commit: String::new(),
        goal: String::new(),
        context: String::new(),
        definition_of_done: String::new(),
        review_type: "deep".into(),
    }
}

#[test]
fn export_import_roundtrip_across_two_repos() {
    let home = state_home("move");
    let source_repo = tmp_dir("source");
    let target_repo = tmp_dir("target");

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
    let read: snapshot::Snapshot =
        serde_yaml::from_str(&std::fs::read_to_string(&snap_path).unwrap()).unwrap();
    snapshot::import(&conn, &read, false).unwrap();

    let ported = db::get(&conn, "ported").unwrap().unwrap();
    assert_eq!(ported.title, "title of ported");
    assert_eq!(db::todos_of(&conn, "ported").unwrap().len(), 1);
    assert_eq!(db::notes_of(&conn, "ported").unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(&source_repo);
    let _ = std::fs::remove_dir_all(&target_repo);
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_dir_all(&home);
}
