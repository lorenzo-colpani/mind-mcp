---
name: plan-refinement
description: Refines pending plans or drafts new ones in two strict phases. Plan mode verifies, discusses, brainstorms, and shows options with zero writes; build mode persists everything through the mind plans tools in one batch. Use when the user says "improve plan", "refine plan", "detail plan", "make a plan", or asks to discuss plans before recording them.
---

# Plan Refinement

Two phases, strictly separated. **Plan mode refines. Build mode persists.**

## Phase A — refine (plan mode, zero writes)

1. **Load state.** `plans_show <name>` for the full record. Check
   `depends_on` both ways — a plan refines only with its dependencies and
   consumers in view.
2. **Ground in code.** Explore the existing machinery, extension points, and
   gaps (explore subagents for speed). Never refine from the plan text alone.
   A plan grounded in code says "wire into known extension points", not
   "design from scratch".
3. **User story first.** Who uses it, what they do, screen by screen, and the
   concrete benefit. No user story — not ready.
4. **Discuss before writing.** For each open point, present one table:
   options, pros, cons, recommendation, why. Show concrete artifacts:
   schema sketches, flow diagrams, example templates, sample screens.
   Ask the user only on real tradeoffs, batched in one `question` call.
   Revisit rejected alternatives with evidence, not preference.
5. **Lock every open point.** A refinement ends with no open design points —
   each becomes a recorded decision with its rejected alternatives.
6. **Present the plan set.** One checklist table: plan, action (close /
   update / new / refine), locked decisions. Wait for the user to say
   **build**.

In plan mode: no file edits, no `plans_*` writes, no commits. The only
artifacts are the discussion and the final checklist.

## Phase B — persist (build mode, one batch)

After the user says build:

1. `plans_note_add` — decisions, rejected options, verified facts. Append-only.
2. `plans_update` — goal, context, definition_of_done, depends_on, order,
   branch, status.
3. `plans_todo_add` — steps that name file paths and are verifiable.
4. No separate `plans.db` commit. It rides in the plan's merge
   commit, per the repo AGENTS.md. Doc fixes a plan touches
   (schema.md, runbooks) ride with the plan's changes too.
5. Report what was written.

## Quality bar

- Todos name file paths and are verifiable.
- DoD is behavioral: a user can do X.
- Dependency edges updated both ways (`depends_on` and `blocks`).
- STE writing throughout: short sentences, active voice, present tense.

## Refine vs create

- **Refine:** never rewrite history; notes are append-only; update goal/DoD
  only when scope actually moves.
- **Create:** place in `order`, set `depends_on`, `review_type: deep` by
  default, then run the same user-story and options pass before todos.

## Splitting a plan

When refinement produces an implementation track inside a decisions plan,
promote children: `plans_add` per concrete work stream, point `depends_on` at
the decisions plan, and re-point downstream plans to the implementation plan
that matters to them. Decisions stay in the parent; code lives in children.

## Stubs

A wish without design goes in as a stub: goal + context only, no todos,
`review_type: none`, name the plan that must land first in `depends_on`.
Refine it through Phase A when its time comes.
