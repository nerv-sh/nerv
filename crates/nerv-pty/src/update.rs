//! Auto-update notification — stripped to a no-op.
//!
//! PRD v0.6 §0.2 lists self-update among the non-goals. The figterm
//! upstream's `check_for_update` shelled out to `fig_install` and
//! `fig_telemetry` to detect install method; both deps are absent
//! from the workspace. Function signature kept so callers in
//! main.rs link without conditional compilation.

use nerv_os::Context;

pub fn check_for_update(_context: &Context) {
    // No auto-update in Nerv (PRD v0.6 §0.2).
}
