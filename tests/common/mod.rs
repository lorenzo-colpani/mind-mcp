//! Shared test fixtures: isolated temp dirs and projects whose
#![allow(dead_code)] // each test binary uses a different subset

//! registries never touch the real `~/.config/opencode/mind`. Every
//! helper takes a tag; parallel tests must never share a state home,
//! or one test's cleanup deletes another test's open database.

use std::path::{Path, PathBuf};

use mind_mcp::db;
use mind_mcp::state::Project;

pub fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mind-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn state_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mind-home-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A project rooted at `root`, registry isolated under `home`.
pub fn project_for(root: &Path, home: &Path) -> Project {
    Project::at(root, home).unwrap()
}

/// A project for `root` with a state home named after the root — unique
/// per test, since every test owns its repo dir.
pub fn project_with_named_home(root: &Path) -> Project {
    let tag = root.file_name().unwrap().to_string_lossy().to_string();
    project_for(root, &state_home(&tag))
}

/// A real git repo, with `url` as its origin remote when given.
pub fn git_repo(tag: &str, url: &str) -> PathBuf {
    let dir = tmp_dir(tag);
    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git init failed in {}", dir.display());
    if !url.is_empty() {
        let ok = std::process::Command::new("git")
            .args(["remote", "add", "origin", url])
            .current_dir(&dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git remote add failed in {}", dir.display());
    }
    dir
}

pub fn plan_row(name: &str) -> db::Plan {
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
