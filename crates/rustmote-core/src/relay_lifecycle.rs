//! Relay bootstrap / check-updates / update / rollback state machines.
//!
//! Implemented in Phase 12 (TASK-012). See spec §3.8 and §5.1.
//! Executes all remote commands via the `session` module's `channel.exec()` —
//! never shells out to local `ssh` and never scps temp scripts.
