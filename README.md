# mind-mcp

Plan registry for projects. One SQLite database per project — `plans.db` at
the project root, machine-local: every agent on the machine writes the same
live file. Two ways in:

- **MCP server** — tools an AI agent calls (`plans_*`)
- **mind CLI** — the same operations for humans

The database is the only live artifact. Portable YAML snapshots exist for
git history and machine moves (`mind export` / `mind import`).

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
mind todo list <plan>   # open todos (in_progress, pending); --all adds done
mind todo add <plan> "step" / mind todo edit <id> --status done
mind note <plan> "decision or finding"
mind rename <old-name> <new-name>   # deps, todos, notes follow
mind remove <name>
mind export [path]      # deterministic YAML snapshot (plans-export.yaml)
mind import <path> --force   # restore; refuses a non-empty registry
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

## Snapshots

The registry is machine-local state, like a dev database. Snapshots carry it
across machines and into git history:

```sh
mind export                # writes plans-export.yaml, byte-stable per state
mind export other.yaml     # any path
mind import plans-export.yaml --force   # replaces the local registry
```

The export is deterministic: the same registry state always produces the
same bytes, so committed snapshots diff cleanly. Restore needs `--force`
unless the local registry is empty.

## Worktrees

Every path derives from the project root, so a session inside a worktree
binds to the worktree's own copy. Set `MIND_REPO` to the canonical checkout
when agents work in trees, so all of them share one live registry:

```sh
MIND_REPO=/path/to/main-checkout mind export
```
