//! RustDesk viewer binary detection and launch.
//!
//! Implemented in Phase 5 (TASK-005). See spec §3.5. The only sanctioned
//! shell-out in the codebase; all args are validated against the allowlists
//! in spec §6.4 before exec.
