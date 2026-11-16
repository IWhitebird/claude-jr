// Runs the real binary against the fake API in bench/mock.py, one file for
// each part of the command line.
//
// Every test gets its own HOME, XDG dirs, project folder and mock server, and
// the child never sees the caller's ANTHROPIC_API_KEY.

mod common;

mod args;
mod clouds;
mod features;
mod fleet;
mod install;
mod mcp;
mod print;
mod providers;
mod sandbox;
mod sessions;
mod subagents;
mod tasks;
mod trust;
mod update;
mod worktrees;
