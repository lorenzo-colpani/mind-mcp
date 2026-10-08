---
name: plans
description: Loads the repo's roadmap context for work on this project's plans. Use when the user mentions plans, the roadmap, plans.db, or before starting any task tied to a plan.
---

# Plan System

The roadmap is one machine-local SQLite file, `plans.db`, at
`~/.config/opencode/mind/<repo-slug>/` — outside every work tree. The
`mind` MCP server serves it (`plans_*` tools). The file is the only
artifact: plans, dependencies, todos, and notes all live inside it. No
markdown, no YAML, nothing generated. The slug derives from the repo's
origin remote, so every clone and tree of the repo shares one registry.
Portable history lives in `plans-export.yaml` snapshots, which the
merger refreshes after every landing.

If the repo has no origin remote and no registry under
`~/.config/opencode/mind/`, this system does not apply here. Stop and
say so.

## Before plan work

1. Call `plans_show` (no arguments) for the board. With a name: the full
   record — goal, context, definition of done, steps, notes.
2. Call `plans_ready` for what is unblocked right now.

Respect `depends_on`. A plan starts only when its dependencies are
`done`.

## While you work

- Set `status: in_progress` with `plans_update` when you start.
- Work through `plans_todo_add` / `plans_todo_edit` (statuses:
  pending, in_progress, done). Todos are the plan's steps.
- Log decisions, findings, and open points with `plans_note_add` as
  they happen. Append-only; the next reader needs this context.
- Everything lives in `plans.db`: context, decisions, findings, todos.
  Write only through `plans_update`, `plans_note_add`, and
  `plans_todo_*`. Never create side files (`discussion.md`,
  `plans/<name>/*.md`). When a design or discussion session ends,
  persist its outcome into the target plan's notes.
- `plans_show <name>` is the whole truth — never read or edit
  `plans.db` by hand.

## On completion

1. Run the review gate named in the plan's `review_type`
   (deep|quick|none): the `code-review` skill.
2. Set `status: done` plus the squash-merge commit with `plans_update`.

`plans.db` never rides commits: the merger lands the
`chore(plans): registry snapshot` commit after every merge.
