---
name: code-review
description: Reviews Rust code at two levels. Classic runs the cargo gates and inspects the diff for craft and correctness issues. Deep spawns two independent reviewer subagents, validates their findings, and aggregates one report. Use when asked to review code, a branch, or before merging a plan.
---

# Code Review

Two levels. Classic always runs. Deep adds independent reviewers.

## Rules for both levels

- Run the gate first: `cargo check --all-targets`, `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`.
- Test code: fix failures and type errors directly. Rerun until green.
- Production code: never modify without explicit approval. Present the issue and the exact proposed diff. Wait for a yes or no.
- Inspect the full branch diff against `master`.

## Classic level

Review the diff for:

- **Smell and repetition.** Duplicated logic. Copy-paste branches.
- **Function size.** Too large to change safely. Not too small: a one-line wrapper is fine when its name earns it.
- **Lifetimes.** Borrow instead of clone. Flag every `.clone()` added only to silence the compiler.
- **Naming.** Names state intent. Flag `data`, `tmp`, `handle_thing2`.
- **Abstractions.** Right level. No speculative generics. No leaky boundaries.
- **Security.** Permission checks at the boundary. Input validated before persistence. No secrets in logs.
- **Business errors.** Wrong rules, wrong state transitions, off-by-one, missed edge cases.
- **Complexity.** Hard only where it must be hard.

Report findings by severity. Include file and line.

## Deep level

Run the classic gate. Then:

1. Spawn two subagents with the task tool. Give each the diff and a distinct angle:
   - **Reviewer A — craft.** Code smells, DRY violations, duplication, function size, naming, lifetimes, abstraction fit.
   - **Reviewer B — bugs and security.** Correctness bugs, broken business rules, security holes, edge cases, error paths, concurrency.
2. Collect both reports.
3. Validate every finding yourself against the real code. Discard false positives.
4. Merge into one report: severity, file, line, proposed fix.
5. Apply the production-code approval rule before any edit.

Finish with: removed tests, added edge cases, approved fixes, remaining risks.

Findings worth keeping go into the plan record with `plans_note_add`.