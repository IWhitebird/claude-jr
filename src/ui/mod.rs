// The interactive terminal UI.
//
// Two threads feed one channel: the agent (events) and a key reader (terminal
// input). The loop blocks on that channel, so an idle session uses no CPU.
// While the agent works it also wakes every 300 ms for the status dot, and
// the terminal is only written to when the picture actually changed.

mod app;
mod commands;
mod events;
mod history;
mod input;
mod keys;
mod md;
mod paste;
mod stats;
#[cfg(test)]
mod stress;
mod table;
mod term;
mod text;
mod view;

use crate::agent::{self, Event};
use app::App;
use crossterm::event::{self as ct, Event as TermEvent};
use std::io;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
use term::Term;

/// How often the status dot moves while the agent works.
const TICK: Duration = Duration::from_millis(300);
/// The fastest the screen is redrawn for streamed text (about 30 frames/s).
/// Key presses are drawn at once.
const FRAME: Duration = Duration::from_millis(33);
/// Lines of history kept to redraw after a resize: far more than a long
/// working day produces (and about 2 MB at most).
const HISTORY_LINES: usize = 20_000;
/// A resize is drawn once the size has stopped changing for this long:
/// dragging a window edge or zooming sends a burst of them.
const RESIZE_SETTLE: Duration = Duration::from_millis(60);

enum Msg {
    Agent(Event),
    Term(TermEvent),
}

/// `bottom`: keep the input on the last rows of the screen.
pub fn run(cfg: agent::Config, bottom: bool) -> io::Result<()> {
    term::restore_on_panic();
    let model = cfg.model.clone();
    let effort = cfg.effort.clone();
    let providers = cfg.providers.clone();
    let (tx, rx) = mpsc::channel();
    let to_ui = tx.clone();
    let agent = agent::spawn(cfg, move |e| {
        let _ = to_ui.send(Msg::Agent(e));
    });

    let mut term = Term::new(bottom)?;
    crate::sys::signals::watch(agent.tools.clone(), true);
    let _ = thread::Builder::new().name("keys".into()).spawn(move || {
        while let Ok(e) = ct::read() {
            if tx.send(Msg::Term(e)).is_err() {
                break;
            }
        }
    });

    let mut app = App::new(agent, model);
    app.effort = effort;
    app.providers = providers;
    app.header();
    // As if the last frame were long ago, so the first one draws at once.
    let mut last_draw = Instant::now().checked_sub(FRAME).unwrap_or_else(Instant::now);
    let mut pending = false;
    // When the terminal last changed size, until that has been drawn.
    let mut resized_at: Option<Instant> = None;
    loop {
        // With a frame held back, sleep until it's due and take everything
        // that arrived meanwhile in one go: streamed text then costs ~30
        // wakeups a second, however small the pieces. Otherwise block until
        // something happens, waking for the status dot while working.
        let msg = if let Some(at) = resized_at {
            match rx.recv_timeout(RESIZE_SETTLE.saturating_sub(at.elapsed())) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else if pending {
            thread::sleep(FRAME.saturating_sub(last_draw.elapsed()));
            None
        } else {
            let wait = if app.work.is_some() { Some(TICK) } else { None };
            match wait.map(|t| rx.recv_timeout(t)) {
                Some(Ok(m)) => Some(m),
                Some(Err(RecvTimeoutError::Timeout)) => None,
                Some(Err(RecvTimeoutError::Disconnected)) => break,
                None => {
                    // Going quiet: give back what the last burst of work needed.
                    crate::sys::mem::trim();
                    match rx.recv() {
                        Ok(m) => Some(m),
                        Err(_) => break,
                    }
                }
            }
        };
        // Handle everything that's waiting, then draw once.
        let mut key = false;
        for m in msg.into_iter().chain(rx.try_iter()) {
            match m {
                Msg::Term(TermEvent::Resize(_, rows)) => {
                    term.resize(rows as usize);
                    resized_at = Some(Instant::now());
                }
                Msg::Term(_) => key = true,
                Msg::Agent(_) => {}
            }
            app.handle(m);
        }
        if app.quit {
            break;
        }
        #[cfg(unix)]
        if std::mem::take(&mut app.suspend) {
            term.suspend();
            crate::sys::signals::stop();
            term.resume()?;
            // The shell has written over the screen: everything again, as after a resize.
            term.resize(Term::size().1);
            resized_at = Some(Instant::now().checked_sub(RESIZE_SETTLE).unwrap_or_else(Instant::now));
            key = true;
        }
        // Nothing is drawn while the size is still changing: every frame
        // would be rewrapped by the terminal a moment later.
        if resized_at.is_some_and(|at| at.elapsed() < RESIZE_SETTLE) {
            continue;
        }
        if !key && resized_at.is_none() && last_draw.elapsed() < FRAME {
            pending = true;
            continue;
        }
        if std::mem::take(&mut app.clear_screen) {
            // /clear has already started the history again, with the header.
            term.clear(true);
        } else if resized_at.take().is_some() {
            // The terminal keeps an old-width copy in its scrollback; wipe
            // that too and draw the kept history again at the new width.
            term.clear(true);
            app.redraw_history();
        }
        let (live, cursor) = app.live();
        term.draw(&mut app.commits, live, cursor)?;
        last_draw = Instant::now();
        pending = false;
    }
    if let Some(tools) = app.agent.tools.get() {
        tools.shutdown();
    }
    Ok(())
}

/// Puts the terminal back to normal, for exits that skip `Term`'s cleanup.
pub fn restore_terminal() {
    term::restore();
}
