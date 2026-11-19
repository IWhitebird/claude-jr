// `claudejr --sessions`: the saved conversations for this folder.

use crate::store::session::Session;

pub fn print() -> Result<(), String> {
    let sessions = Session::list();
    if sessions.is_empty() {
        return crate::say("No saved conversations in this directory.");
    }
    for s in sessions.iter().rev() {
        let name = s.name.as_deref().unwrap_or("-");
        let prompt: String = s.first_prompt.lines().next().unwrap_or("").chars().take(60).collect();
        crate::say(format_args!("{:<14} {:<20} {:>4} msgs  {prompt}", s.id, name, s.messages))?;
    }
    crate::say("\nResume with: claudejr -r NAME_OR_ID   (or -c for the newest)")
}
