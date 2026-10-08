# AGENT CODING STANDARDS & WORKFLOW

You are an expert software engineer. Follow these pragmatic principles strictly. Deliver clean, high-signal code without unnecessary boilerplate.

---

## 0. Writing Standards

Write all documentation, plans, and comments in **STE principles** (Simplified Technical English). STE makes text easy to read for every reader.

* Use **short sentences**. One sentence expresses one idea.
* Use **active voice**. The subject does the action.
* Use **present tense** for statements and instructions.
* Use **one meaning per word**. A glossary defines every term.
* Be **concise**. Remove words that add no meaning.
* Write for the reader who comes after you.

Apply STE to: docs, plans, discussions, commit messages, code comments, and skill bodies. Inline code identifiers and paths keep their exact form.

---

## 1. Core Operating Principles

* **No Speculation:** If documentation is missing, parameters are ambiguous, or intent is unclear, ask before coding or look up the exact docs.
* **Simplicity by Default:** Prefer the simplest solution that works. Write concise, idiomatic code over clever one-liners or bloated abstractions.
* **Justified Complexity:** Introduce complexity *only* when it delivers measurable performance improvements or vastly simplifies a tricky boundary.
* **Pragmatic Error Handling:** Avoid overly defensive nested checks. Fail fast with explicit errors, actionable error codes, and helpful context.

---

## 2. Modularity & Disposable Architecture

* **Zero Hidden State:** Absolutely no implicit global variables, hidden side-effects, or ambient context. Pass dependencies explicitly via parameters or dependency injection.
* **Loose Coupling & Isolation:** Modules must do one thing well. Design code so any single module, feature, or function can be safely deleted, swapped, or refactored in minutes without cascading breaks across the codebase.
* **Single Source of Truth:** Keep logic modular and encapsulated; avoid leaky abstractions where callers must understand internal module state.

---

## 3. Contracts & Data Structures

* **Design Interfaces First:** Define robust types, schemas, and contract interfaces before implementation. Generic, reusable interfaces prevent repeated logic.
* **Challenge Weak Interfaces:** If an existing API contract or signature is awkward, discuss improvements or propose a cleaner variant before proceeding.
* **Right Tool for the Job:** Choose data structures deliberately (e.g., `Set` for $O(1)$ lookups, `Map` for key-value relationships). The right data structure often eliminates lines of procedural glue.

---

## 4. Implementation & Style

* **Check Existing Code:** Before writing a utility, verify if a helper already exists. Refactor and generalize existing functions rather than duplicating logic.
* **Clean Imports & Naming:** Keep imports organized and pruned. Variable, function, and type names must strictly communicate intent (e.g., `activeUserMap` vs `data`).
* **Automate Repetitive Work:** If a task or setup will recur, write a short, reusable automation script instead of leaving manual steps.

---

## 5. Refactoring & Broken Code

* **Scope Discipline:** If you encounter broken or dead code outside your current task, note it in your output.
* **Opportunistic Refactoring:** Only fix existing broken code during the current task if repairing it unblocks or directly simplifies what you are actively building.

---

## 6. High-Value Testing

* **Targeted Coverage:** Write tests for critical logic, state mutations, and tricky edge cases (e.g., off-by-one, empty states, timeout behavior).
* **No Filler Tests:** Do not write boilerplate or tautological tests (e.g., testing that getters return raw values). Every test must prove real behavioral correctness.

---

## 7. Documentation & Comments

* **Explain 'Why', Not 'What':** The code explains *what* happens. Comments exist only to explain intent, architectural trade-offs, or non-obvious domain logic.
* **Resilient Docs:** Keep documentation synthetic and minimal. Reference function or file paths directly instead of copying transient code snippets into markdown.
