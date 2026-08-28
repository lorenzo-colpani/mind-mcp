# mind-mcp

Plan registry for projects. One SQLite database per project — `plans.db` at
the project root, committed to git. Two ways in:

- **MCP server** — tools an AI agent calls (`plans_*`)
- **mind CLI** — the same operations for humans

The database is the only artifact. No generated markdown, no YAML snapshots.
A fresh clone already holds the full registry: plans, dependencies, todos,
notes, and done history.

## Install

```sh
cargo install --path . --bin mind
```

## Use

`cd` into a project, then:

```sh
mind board              # active plans, by run order
mind board --all        # include finished work
mind tree               # graph of active work
mind show <plan>        # full record: goal, context, DoD, todos, notes
mind ready              # unblocked plans
mind add <name> "<title>" --goal "..." --definition-of-done "..."
mind update <name> --status done --merge-commit abc1234
mind todo add <plan> "step" / mind todo edit <id> --status done
mind note <plan> "decision or finding"
mind rename <old-name> <new-name>   # deps, todos, notes follow
mind remove <name>
```

Add `--json` to read commands for scripting.

## Plan shape

A plan record: `title`, `goal`, `context`, `definition_of_done`,
`review_type` (`deep|quick|none`), `branch`, `status`
(`pending|in_progress|partial|done`), `merge_commit`, run `order`,
`depends_on`. Steps live as todos (`pending|in_progress|done`). Decisions,
findings, and open points go to the plan's append-only notes.

## First run on an old project

`mind adopt` migrates a legacy setup in one step. It copies the old hidden
database (`~/.local/share/mind-mcp/`), imports `plans/<name>/` folders
(README sections become plan fields, steps become todos, discussion entries
become notes), then deletes the folders, `plans.md`, and `plans.yaml`.
Refuses to run when `plans.db` already exists.

## Git

Commit `plans.db` with plan changes, separately from feature work. The file
is small; binary means no line diffs — read state with `mind board` /
`mind show`.
