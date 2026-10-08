---
name: interface-design
description: Explores competing designs for a new API, trait, struct, module boundary, or route shape. Spawns two to three subagents, each proposing a different interface with a concrete example, pros, and cons. Use before committing to a contract.
---

# Interface Design

Contracts come first. Explore before you commit.

## Process

1. **Frame.** State the problem, the callers, and the constraints. One paragraph.
2. **Spawn.** Launch two to three subagents with the task tool. Give each the frame and a distinct philosophy, for example:
   - **Minimal surface.** Fewest functions, smallest types.
   - **Type-driven.** Encode rules in the type system.
   - **Pragmatic.** Optimize for the next caller. Accept some duplication.
3. Each subagent returns: the interface as code, one usage example, pros, cons.
4. **Compare.** Merge results into one table: ergonomics, extensibility, test cost, coupling.
5. **Recommend.** State your pick and why. Show the runner-up trade-off. The user decides.

## Ground rules

- Show real signatures, not descriptions.
- Pros and cons stay concrete: "callers import two traits", not "a bit complex".
- Challenge an existing awkward contract instead of preserving it.
- Log the picked contract and the runner-up trade-off with
  `plans_note_add`. No side files.