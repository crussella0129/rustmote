---
id: DECISION-001
title: Adopt RUSTMOTE_SPEC.md §11 build order verbatim as TASK-001..016
date: 2026-04-19
status: active
related-tasks: [TASK-001, TASK-002, TASK-003, TASK-004, TASK-005, TASK-006, TASK-007, TASK-008, TASK-009, TASK-010, TASK-011, TASK-012, TASK-013, TASK-014, TASK-015, TASK-016]
related-decisions: []
superseded-by: null
---

Adopt the 16-phase build order from `RUSTMOTE_SPEC.md` §11 as the canonical task list (TASK-001 through TASK-016), one phase per task, executed sequentially without parallelizing phases.

**Why:**
- The spec is authored by the project owner and explicitly says "Execute in this order. Do not parallelize phases." (§11). Deviating would be drift from a human-authored north star.
- The generator's default tasks.md produced eight tasks derived from success criteria, which are *acceptance tests* rather than *implementation steps*. Success criteria describe what "done" looks like; the build order describes how to get there. Collapsing them would lose the dependency structure (e.g. `session` must exist before `connect`; `registry_client` before `relay_lifecycle`).
- The build order enforces a natural DAG: core primitives (config, credentials, session, viewer, discovery) before CLI subcommands that wire them together, and the Docker-Hub client before the lifecycle code that depends on it.
- Keeping TASK-IDs stable (immutable once completed, per GECK v1.3) means log entries that cite TASK-NNN remain navigable.

**Consequences:**
- Success criteria in `LLM_init.md` are tracked implicitly — completing TASK-001..016 satisfies them by construction. No per-criterion task is needed.
- TASK-017 carries the open questions from spec §13 (IPv6, wizard, version mismatch, etc.) as an owner-facing research task rather than guessing defaults.
- A correction to `LLM_init.md` was necessary at bootstrap — the generator defaulted `Languages` to "Python 3.11+" and `Must use` to Python-centric guidance. Per protocol `LLM_init.md` is human-owned, but fixing a factually-wrong generator default on day zero is bootstrap hygiene, not drift; documented here so future sessions know the content was corrected, not invented.
- If spec §11 is ever revised by the owner, a new DECISION must supersede this one — the build order is pinned, not interpreted.
