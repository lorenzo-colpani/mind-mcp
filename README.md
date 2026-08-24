# mind-mcp

Plan registry for projects. One SQLite database per project, two ways in:

- **MCP server** — tools an AI agent calls (`plans_*`)
- **mind CLI** — the same operations for humans

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
mind tree <plan>        # one plan: its deps and dependents
mind show <plan>        # full record
mind ready              # unblocked plans
mind add <name> "<title>" [--depends-on <plan>]
mind update <name> --status done --merge-commit abc1234
mind rename <old-name> <new-name>   # dependency edges follow
mind remove <name>
```

Add `--json` to any read command for scripting.

Mutations regenerate `plans.md` and `plans.yaml` in the project root, so the
agent's view and yours stay identical.
