//! Global brain file (`~/.config/opencode/brain.md`): cross-project lessons.
//!
//! Markdown is the store. One lesson per line with an inline id, so humans
//! can read and edit the file directly.

use std::fmt::Write as _;

use anyhow::Context;

#[derive(Clone, Debug)]
pub struct Lesson {
    pub id: i64,
    pub tag: String,
    pub text: String,
}

const BRAIN_HEADER: &str = "\
# Brain

Global lessons. One per line. Format: `- [tag] lesson <!--id:N-->`.
Short sentences. Active voice. Present tense.
Repo-specific findings belong in each repo's docs/FINDINGS.md, not here.

";

pub fn parse_brain(text: &str) -> Vec<Lesson> {
    text.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("- [")?;
            let (tag, after) = rest.split_once("]")?;
            let (text, id_part) = after.trim().split_once("<!--id:")?;
            let id: i64 = id_part.trim_end_matches("-->").trim().parse().ok()?;
            Some(Lesson {
                id,
                tag: tag.to_string(),
                text: text.trim().to_string(),
            })
        })
        .collect()
}

pub fn read_brain() -> anyhow::Result<Vec<Lesson>> {
    let path = crate::state::brain_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(parse_brain(&text)),
        Err(_) => Ok(Vec::new()),
    }
}

fn brain_header_present(text: &str) -> bool {
    text.contains("Global lessons")
}

fn write_brain(lessons: &[Lesson]) -> anyhow::Result<()> {
    let mut out = String::new();
    let existing = std::fs::read_to_string(crate::state::brain_path())
        .map(|t| {
            if brain_header_present(&t) {
                t
            } else {
                BRAIN_HEADER.to_string()
            }
        })
        .unwrap_or_else(|_| BRAIN_HEADER.to_string());
    out.push_str(
        existing
            .lines()
            .take_while(|l| !l.starts_with("- ["))
            .collect::<Vec<_>>()
            .join("\n")
            .as_str(),
    );
    out.push('\n');
    for l in lessons {
        let _ = writeln!(out, "- [{}] {} <!--id:{}-->", l.tag, l.text, l.id);
    }
    crate::state::write_file(&crate::state::brain_path(), &out)
}

pub fn brain_add(tag: &str, text: &str) -> anyhow::Result<Lesson> {
    let mut lessons = read_brain()?;
    let next = lessons.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    let lesson = Lesson {
        id: next,
        tag: tag.to_string(),
        text: text.to_string(),
    };
    lessons.push(lesson.clone());
    write_brain(&lessons)?;
    Ok(lesson)
}

pub fn brain_edit(id: i64, tag: Option<&str>, text: Option<&str>) -> anyhow::Result<Lesson> {
    let mut lessons = read_brain()?;
    let lesson = lessons
        .iter_mut()
        .find(|l| l.id == id)
        .context(format!("unknown lesson id {id}"))?;
    if let Some(t) = tag {
        lesson.tag = t.to_string();
    }
    if let Some(t) = text {
        lesson.text = t.to_string();
    }
    let updated = lesson.clone();
    write_brain(&lessons)?;
    Ok(updated)
}

pub fn brain_remove(id: i64) -> anyhow::Result<()> {
    let mut lessons = read_brain()?;
    let before = lessons.len();
    lessons.retain(|l| l.id != id);
    anyhow::ensure!(lessons.len() < before, "unknown lesson id {id}");
    write_brain(&lessons)
}
