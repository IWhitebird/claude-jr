// Clean exit on SIGINT, SIGTERM and SIGHUP (closing the terminal).
//
// Commands and background jobs run in their own process groups, so a signal
// sent to claude-jr doesn't reach them. Without this they'd keep running
// after we're gone.

use crate::tools::Tools;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::sync::{Arc, OnceLock};

/// Watches for exit signals on a small thread. On one, stops everything we
/// started, puts the terminal back if `terminal` is set, and exits.
pub fn watch(tools: Arc<OnceLock<Arc<Tools>>>, terminal: bool) {
    let Ok(mut signals) = Signals::new([SIGINT, SIGTERM, SIGHUP]) else { return };
    std::thread::spawn(move || {
        if let Some(sig) = signals.forever().next() {
            if let Some(t) = tools.get() {
                t.shutdown();
            }
            if terminal {
                crate::ui::restore_terminal();
            }
            std::process::exit(128 + sig);
        }
    });
}
