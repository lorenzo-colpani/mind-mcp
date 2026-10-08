---
name: stress-test
description: Challenges an idea, design, or plan through structured argument before any code is written. Use when the user wants a second opinion on a proposal, wants weak points surfaced, or must choose between options.
---

# Stress Test

A dialogue skill. You argue, the user decides. Do not write production code.

## Method

1. **Restate.** Say the proposal back in your own words. List hidden assumptions.
2. **Attack the edges.** Empty input, missing data, timeouts, concurrency, scale, failure recovery. Find the case that breaks it.
3. **Argue against it.** Give the strongest counter-position. Why might this fail or cost too much?
4. **Offer alternatives.** Cheaper, simpler, or safer shapes that solve the same problem.
5. **Score it.** End with three lists:
   - **Holds.** Claims that survive attack.
   - **Fragile.** Claims with real risk.
   - **Change.** Concrete adjustments before build.

Ask sharp questions. One idea per question. Push until the design is either solid or clearly broken.

## Outcome

Log the final verdict (Holds / Fragile / Change) with `plans_note_add`
into the relevant plan. No side files — the plan notes are the record.