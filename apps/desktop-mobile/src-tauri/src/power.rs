//! Power state reported by the mobile shells, for scheduling background work.
//!
//! Windows and Linux can read this from Rust (see `search::on_battery`). Android and iOS expose
//! it only to app code, so the webview asks the native plugin (`getPower`) and hands the answer
//! to [`power_report`]. Readers see "unknown" until the first report and never block on one.
//!
//! The search worker uses [`on_battery`] for its "wait while on battery" setting and the sync
//! worker stretches its idle poll while [`saver`] is on, on every platform that reports it.

use std::sync::atomic::{AtomicBool, AtomicI8, Ordering};

const UNKNOWN: i8 = -1;

// Only read on platforms without their own source; written everywhere.
#[cfg_attr(not(mobile), allow(dead_code))]
static ON_BATTERY: AtomicI8 = AtomicI8::new(UNKNOWN);
static SAVER: AtomicBool = AtomicBool::new(false);

/// Records what the platform reports. `on_battery` is `None` when it cannot tell.
#[tauri::command]
pub fn power_report(on_battery: Option<bool>, saver: bool) {
    ON_BATTERY.store(on_battery.map_or(UNKNOWN, i8::from), Ordering::Relaxed);
    SAVER.store(saver, Ordering::Relaxed);
}

/// The last reported power source: `Some(true)` on battery, `None` when unknown.
#[cfg_attr(not(mobile), allow(dead_code))]
pub fn on_battery() -> Option<bool> {
    match ON_BATTERY.load(Ordering::Relaxed) {
        UNKNOWN => None,
        v => Some(v == 1),
    }
}

/// Whether the OS battery saver was on at the last report.
pub fn saver() -> bool {
    SAVER.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_replace_each_other_and_unknown_stays_unknown() {
        power_report(Some(true), true);
        assert_eq!(on_battery(), Some(true));
        assert!(saver());
        power_report(Some(false), false);
        assert_eq!(on_battery(), Some(false));
        assert!(!saver());
        power_report(None, false);
        assert_eq!(on_battery(), None);
    }
}
