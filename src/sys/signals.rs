// A clean exit when told to stop: SIGINT, SIGTERM and SIGHUP (closing the
// terminal) on Unix; Ctrl+C, Ctrl+Break and closing the console on Windows.
//
// Commands and background jobs run apart from claude-jr (see os.rs), so a
// signal sent to it doesn't reach them. Without this they'd keep running
// after we're gone.

use crate::tools::Tools;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

/// Watches for being told to stop. Then stops everything we started, puts
/// the terminal back if `terminal` is set, and exits.
#[cfg(unix)]
pub fn watch(tools: Arc<OnceLock<Arc<Tools>>>, terminal: bool) {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGINT, SIGTERM, SIGHUP]) else { return };
    let _ = std::thread::Builder::new().name("signals".into()).spawn(move || {
        if let Some(sig) = signals.forever().next() {
            leave(&tools, terminal, 128 + sig);
        }
    });
}

#[cfg(windows)]
pub fn watch(tools: Arc<OnceLock<Arc<Tools>>>, terminal: bool) {
    on_console_event(Box::new(move || leave(&tools, terminal, 130)));
}

fn leave(tools: &OnceLock<Arc<Tools>>, terminal: bool, code: i32) -> ! {
    if let Some(t) = tools.get() {
        t.shutdown();
    }
    if terminal {
        crate::ui::restore_terminal();
    }
    std::process::exit(code)
}

/// For a run that winds down by itself when told to stop (a background
/// task): sets `flags` and lets it carry on to its end. On Windows, where one
/// process can't signal another, a file appearing at `stop_file` means the same.
#[cfg(unix)]
pub fn flag(flags: Vec<Arc<AtomicBool>>, stop_file: Option<PathBuf>) {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let _ = stop_file;
    for signal in [SIGTERM, SIGINT, SIGHUP] {
        for flag in &flags {
            let _ = signal_hook::flag::register(signal, flag.clone());
        }
    }
}

#[cfg(windows)]
pub fn flag(flags: Vec<Arc<AtomicBool>>, stop_file: Option<PathBuf>) {
    use std::sync::atomic::Ordering;
    let raise = move || flags.iter().for_each(|f| f.store(true, Ordering::Relaxed));
    if let Some(file) = stop_file {
        let raise = raise.clone();
        let _ = std::thread::Builder::new().name("stop-file".into()).spawn(move || {
            while !file.exists() {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            raise();
        });
    }
    on_console_event(Box::new(raise));
}

/// Runs `then` (on a thread Windows makes for it) on Ctrl+C, Ctrl+Break or
/// when the console is closed.
#[cfg(windows)]
fn on_console_event(then: Box<dyn Fn() + Send + Sync>) {
    static THEN: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    unsafe extern "system" fn handler(_event: u32) -> i32 {
        if let Some(then) = THEN.get() {
            then();
        }
        1 // handled: don't end the process on the spot
    }
    if THEN.set(then).is_ok() {
        // SAFETY: registers a function that lives as long as the program.
        unsafe { super::os::win::SetConsoleCtrlHandler(Some(handler), 1) };
    }
}
