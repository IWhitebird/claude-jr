// Key presses: the prompts that take them first (approvals, questions,
// pickers, the key prompt), then editing, and the settings some of them change.

use super::app::{App, Prompt};
use super::commands::menu_matches;
use super::text::{DIM, RED};
use crate::agent::{Answer, Command};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub(super) const QUIT_WINDOW: Duration = Duration::from_millis(1500);
/// How long an approval prompt ignores answer keys after it appears.
const APPROVE_GUARD: Duration = Duration::from_millis(400);

/// Keys pressed this soon after an approval appears were typed for something else.
pub(super) fn armed(since: Instant, now: Instant) -> bool {
    now.duration_since(since) >= APPROVE_GUARD
}

impl App {
    pub(super) fn on_key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match self.prompts.front() {
            Some(Prompt::Approve { .. }) => return self.on_approve_key(k),
            Some(Prompt::Question { .. }) => {
                if k.code == KeyCode::Esc || (ctrl && k.code == KeyCode::Char('c')) {
                    self.answer(String::new());
                    return self.cancel();
                }
            }
            Some(Prompt::Rewind(points)) => {
                match k.code {
                    KeyCode::Char(c @ '1'..='9') => {
                        if let Some(&(turn, _)) = points.get(c as usize - '1' as usize) {
                            self.prompts.pop_front();
                            let _ = self.agent.commands.send(Command::Rewind(Some(turn)));
                        }
                    }
                    KeyCode::Esc => {
                        self.prompts.pop_front();
                    }
                    _ => {}
                }
                return;
            }
            Some(Prompt::Resume(sessions)) => {
                match k.code {
                    KeyCode::Char(c @ '1'..='9') => {
                        if let Some((id, _)) = sessions.get(c as usize - '1' as usize) {
                            let pick = crate::store::session::Pick::Named(id.clone());
                            self.prompts.pop_front();
                            let _ = self.agent.commands.send(Command::Resume(pick));
                        }
                    }
                    KeyCode::Esc => {
                        self.prompts.pop_front();
                    }
                    _ => {}
                }
                return;
            }
            Some(Prompt::Model) => {
                match k.code {
                    KeyCode::Char(c @ '1'..='9') => {
                        if let Some((model, _)) = crate::api::CATALOGUE.get(c as usize - '1' as usize) {
                            self.prompts.pop_front();
                            self.set_model(&crate::api::cloud::same_door(&self.model, model));
                        }
                    }
                    KeyCode::Esc => {
                        self.prompts.pop_front();
                    }
                    _ => {}
                }
                return;
            }
            Some(Prompt::Settings) => {
                match k.code {
                    KeyCode::Char(c @ '1'..='9') => self.change_setting(c as usize - '1' as usize),
                    KeyCode::Esc => {
                        self.prompts.pop_front();
                    }
                    _ => {}
                }
                return;
            }
            Some(Prompt::Key) => match k.code {
                KeyCode::Enter => return self.save_key(),
                KeyCode::Esc => {
                    self.prompts.pop_front();
                    self.input.clear();
                    return self.note("No key set. Type /login when you're ready.", DIM);
                }
                _ => {}
            },
            None => {}
        }

        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let text_cols = self.cols.saturating_sub(6);
        // With the `/` command list open: arrows pick, tab completes, enter runs.
        let menu = if self.prompts.is_empty() { menu_matches(self.input.text(), &self.skills) } else { Vec::new() };
        if !menu.is_empty() {
            let at = self.menu_at.min(menu.len() - 1);
            let picked = format!("/{}", menu[at].0);
            match k.code {
                KeyCode::Up => return self.menu_at = if at == 0 { menu.len() - 1 } else { at - 1 },
                KeyCode::Down => return self.menu_at = (at + 1) % menu.len(),
                KeyCode::Tab => {
                    self.input.clear();
                    self.input.insert(&format!("{picked} "));
                    return self.menu_at = 0;
                }
                KeyCode::Enter if !(shift || alt) => {
                    self.input.take();
                    self.menu_at = 0;
                    return self.submit(picked);
                }
                _ => self.menu_at = 0,
            }
        }
        match k.code {
            KeyCode::Char('c') if ctrl => self.ctrl_c(),
            KeyCode::Char('d') if ctrl && self.input.is_empty() => self.quit = true,
            KeyCode::Char('z') if ctrl && cfg!(unix) => self.suspend = true,
            KeyCode::Char('j') if ctrl => self.input.insert("\n"),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char('u') if ctrl => self.input.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.input.kill_to_end(),
            KeyCode::Char('w') if ctrl => self.input.delete_word(),
            // Put what's typed aside to send something else first; it comes back
            // after that message is sent, or at once with ctrl+s again.
            KeyCode::Char('s') if ctrl => match self.stash.take() {
                Some(stashed) if self.input.is_empty() => self.input.insert(&stashed),
                stashed => {
                    self.stash = stashed;
                    if !self.input.is_empty() {
                        let text = self.input.take_secret();
                        // A second stash is kept with the first rather than lost.
                        self.stash = Some(match self.stash.take() {
                            Some(older) => format!("{older}\n{text}"),
                            None => text,
                        });
                    }
                }
            },
            // The terminal's own paste arrives as a paste event; ctrl+v reads
            // the clipboard itself, so it can take an image as well as text.
            KeyCode::Char('v') if ctrl => match crate::store::images::clipboard() {
                Ok(crate::store::images::Clip::Image(path)) => self.input.insert(&format!("{} ", path.display())),
                Ok(crate::store::images::Clip::Text(text)) => self.paste(&text),
                Err(e) => self.note(&format!("{e}."), DIM),
            },
            KeyCode::Backspace if ctrl || alt => self.input.delete_word(),
            KeyCode::Char(c) if !ctrl => {
                let mut buf = [0u8; 4];
                self.input.insert(c.encode_utf8(&mut buf));
            }
            KeyCode::Enter if shift || alt => self.input.insert("\n"),
            KeyCode::Enter => {
                if !self.input.continues_line() {
                    let text = self.input.take();
                    if matches!(self.prompts.front(), Some(Prompt::Question { .. })) {
                        self.answer(text);
                    } else if !text.trim().is_empty() {
                        self.submit(text);
                        if let Some(stashed) = self.stash.take() {
                            self.input.insert(&stashed);
                        }
                    }
                }
            }
            KeyCode::Esc => self.cancel(),
            KeyCode::BackTab => {
                let mode = &self.agent.mode;
                mode.set(mode.get().next());
            }
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up => self.input.up(text_cols),
            KeyCode::Down => self.input.down(text_cols),
            _ => {}
        }
    }

    pub(super) fn on_approve_key(&mut self, k: KeyEvent) {
        let ctrl_c = k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
        let always = matches!(k.code, KeyCode::Esc | KeyCode::BackTab) || ctrl_c;
        if !always && !armed(self.prompt_since, Instant::now()) {
            return;
        }
        let answer = match k.code {
            KeyCode::Char('y' | 'Y' | '1') => Answer::Yes,
            KeyCode::Char('a' | 'A' | '2') => Answer::Always,
            KeyCode::Char('n' | 'N' | '3') | KeyCode::Esc => Answer::No,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancel();
                Answer::No
            }
            KeyCode::BackTab => {
                let mode = &self.agent.mode;
                return mode.set(mode.get().next());
            }
            _ => return,
        };
        if let Some(Prompt::Approve { reply, .. }) = self.prompts.pop_front() {
            let _ = reply.send(answer);
        }
        self.prompt_since = Instant::now();
    }

    /// Answers the question at the front of the queue.
    /// Answers the question at the front of the queue. A number on its own
    /// picks that option; anything else is sent as typed.
    pub(super) fn answer(&mut self, text: String) {
        if let Some(Prompt::Question { reply, options, .. }) = self.prompts.pop_front() {
            let picked = text.trim().parse::<usize>().ok().and_then(|n| options.get(n.wrapping_sub(1)));
            let _ = reply.send(picked.cloned().unwrap_or(text));
        }
    }

    /// Moves a setting to its next value and saves it.
    pub(super) fn change_setting(&mut self, i: usize) {
        let Some(setting) = SETTINGS.get(i) else { return };
        let now = current_setting(setting);
        let at = setting.choices.iter().position(|c| *c == now).map_or(0, |p| p + 1);
        let next = setting.choices[at % setting.choices.len()];
        let value = match next {
            // The model's own default: no value in the file.
            "default" => serde_json::Value::Null,
            "on" => serde_json::Value::Bool(true),
            "off" => serde_json::Value::Bool(false),
            v => serde_json::Value::String(v.to_string()),
        };
        if let Err(e) = crate::store::config::set_user_setting(setting.key, value) {
            return self.note(&format!("Couldn't save the setting: {e}"), RED);
        }
        if setting.key == "effort" {
            self.set_effort(next);
        }
    }

    pub(super) fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
        let _ = self.agent.commands.send(Command::Model(self.model.clone()));
        self.note(&format!("Model set to {}. {}", self.model, crate::api::describe(model)), DIM);
    }

    /// /effort: how hard the model thinks. The API's default for the model is
    /// used until one is picked. Changing it mid-conversation means the next
    /// request misses the prompt cache once.
    pub(super) fn set_effort(&mut self, arg: &str) {
        const LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
        match arg {
            "" => {
                let now = self.effort.as_deref().unwrap_or("the model's default");
                self.note(&format!("Effort: {now}. Choose with /effort {} or default.", LEVELS.join("|")), "");
            }
            "default" => {
                self.effort = None;
                let _ = self.agent.commands.send(Command::Effort(None));
                self.note("Effort set to the model's default.", DIM);
            }
            level if LEVELS.contains(&level) => {
                self.effort = Some(level.to_string());
                let _ = self.agent.commands.send(Command::Effort(self.effort.clone()));
                self.note(&format!("Effort set to {level}."), DIM);
            }
            other => self.note(&format!("Unknown effort \"{other}\". Use {} or default.", LEVELS.join(", ")), RED),
        }
    }

    /// Saves the key typed into the key prompt and hands it to the agent.
    pub(super) fn save_key(&mut self) {
        let key = self.input.take_secret().trim().to_string();
        if key.is_empty() {
            return;
        }
        self.prompts.pop_front();
        match crate::store::config::save_api_key(&key) {
            Ok(path) => self.note(&format!("Saved your API key to {}.", path.display()), DIM),
            Err(e) => self.note(&format!("Couldn't save the key ({e}); it's used for this session only."), RED),
        }
        if !key.starts_with("sk-ant-") {
            self.note("That doesn't look like an Anthropic key (they start with sk-ant-). /login to change it.", RED);
        }
        let _ = self.agent.commands.send(Command::SetKey(key));
        self.has_key = true;
        self.send_queued();
    }

    pub(super) fn ctrl_c(&mut self) {
        if !self.input.is_empty() {
            self.input.clear();
        } else if self.work.is_some() {
            self.cancel();
        } else if self.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
            self.quit = true;
        } else {
            self.quit_armed = Some(Instant::now());
        }
    }

    pub(super) fn cancel(&mut self) {
        if self.work.is_some() {
            self.agent.cancel.store(true, Ordering::Relaxed);
            self.set_activity("stopping".into());
        }
    }
}

/// One line of /settings: the config key, and the values it cycles through
/// (the first is the default).
pub(super) struct Setting {
    pub(super) label: &'static str,
    pub(super) key: &'static str,
    pub(super) choices: &'static [&'static str],
}

pub(super) const SETTINGS: [Setting; 6] = [
    Setting { label: "Input at the bottom of the screen", key: "layout", choices: &["bottom", "inline"] },
    Setting {
        label: "Model for new sessions",
        key: "model",
        choices: &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1"],
    },
    Setting { label: "Effort", key: "effort", choices: &["default", "low", "medium", "high", "xhigh", "max"] },
    Setting { label: "Start in mode", key: "mode", choices: &crate::agent::Mode::NAMES },
    Setting { label: "Web search and fetch", key: "web_tools", choices: &["on", "off"] },
    Setting { label: "Sandbox (writes only in the project)", key: "sandbox.enabled", choices: &["off", "on"] },
];

/// A setting's value in the user's config file, or its default.
pub(super) fn current_setting(s: &Setting) -> &'static str {
    let root = crate::store::config::user_settings();
    let value = s.key.split('.').fold(&root, |node, part| &node[part]);
    let text = match value {
        serde_json::Value::Bool(true) => "on",
        serde_json::Value::Bool(false) => "off",
        serde_json::Value::String(v) => v.as_str(),
        _ => s.choices[0],
    };
    s.choices.iter().find(|c| **c == text).copied().unwrap_or(s.choices[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_keys_wait_for_the_guard() {
        let t = Instant::now();
        let at = |ms| t + Duration::from_millis(ms);
        assert!(!armed(t, at(0)));
        assert!(!armed(t, at(399)));
        assert!(armed(t, at(400)));
        assert!(armed(t, at(2000)));
    }
}
