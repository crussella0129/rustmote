---
id: DECISION-002
title: Bump MSRV from 1.75 to 1.85 to match edition-2024 ecosystem
date: 2026-04-19
status: active
related-tasks: [TASK-004]
related-decisions: []
superseded-by: null
---

Raise `workspace.package.rust-version` from `1.75` to `1.85`, update the CI matrix MSRV job accordingly, and amend `RUSTMOTE_SPEC.md` §2 and §7.4 to match. Supersedes the spec's original 1.75 floor.

**Why:**
- As of 2026-04, 13 crates in Rustmote's transitive dep tree have migrated to edition 2024 — confirmed by scanning the local registry cache: `assert_cmd`, `base64ct`, `clap`, `clap_builder`, `clap_derive`, `clap_lex`, `comfy-table`, `getrandom`, `globset`, `hashbrown`, `home`, `ignore`, `indexmap`. Edition 2024 was stabilized in rustc 1.85 (Feb 2025), and rustc 1.75 (Dec 2023) predates it — so 1.75 cannot parse these manifests and the MSRV CI job fails with `feature edition2024 is required` before the code even compiles.
- Pinning all 13 crates to pre-edition-2024 versions in `Cargo.lock` is not sustainable: the resolver under 1.75 does not honor transitive `rust-version` fields, so any future `cargo update` in a fresh checkout re-promotes to latest and re-breaks the MSRV job. That would make the lockfile a load-bearing config file — precisely the fragility the MSRV job is meant to prevent.
- Keeping 1.75 as the MSRV was originally motivated in the spec by native async-in-trait stabilizing there (§6.5-adjacent). That motivation is satisfied equally by 1.85, which is also the floor for every modern crate we already depend on (russh, reqwest, clap 4.6, keyring 3, tokio recent).
- The realistic alternatives — drop the MSRV job, or pin the ecosystem — both hurt the deterministic-build guarantee more than a one-time MSRV bump.

**Consequences:**
- `Cargo.toml` workspace `rust-version = "1.85"`, `ci.yml` MSRV job runs `1.85`, `RUSTMOTE_SPEC.md` §2 (code block at line 87) and §7.4 (CI matrix line) updated in lockstep.
- Workspace `resolver = "3"` (stabilized in Cargo 1.84) is required alongside the bump — without it the resolver ignores transitive `rust-version` fields and promotes deps whose own MSRV exceeds ours (observed on first push: `home@0.5.12` requires 1.88, `icu_*@2.2.0` requires 1.86). Resolver v3 downgrades those to the newest 1.85-compatible version automatically, making the MSRV job stable without per-crate pins in `Cargo.lock`.
- v0.1.0 ships with MSRV 1.85 in `CHANGELOG.md` and README.
- Future MSRV bumps get their own DECISION-NNN rather than being handled inline.
- No source code changes are required — we were never using features post-1.75 gated on the MSRV; the bump is purely an ecosystem-compatibility move.
