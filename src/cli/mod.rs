// The command line: one file for each thing you can type.
//
//   args.rs      the flags and the usage text
//   start.rs     claudejr and claudejr -p: start-up checks, then the UI or print
//   print.rs     -p: one prompt, the reply printed
//   sessions.rs  --sessions
//   update.rs    --update
//   task.rs      claudejr task ...
//   fleet.rs     claudejr fleet ...
//   mcp.rs       claudejr mcp: tasks served to another agent
//   install.rs   claudejr install / uninstall: setting that up for an agent

pub mod args;
pub mod fleet;
pub mod install;
pub mod mcp;
pub mod print;
pub mod sessions;
pub mod start;
pub mod task;
pub mod update;
