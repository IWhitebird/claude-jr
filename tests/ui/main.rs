// The interactive UI, end to end: the real binary runs in a pseudo-terminal
// (made by util-linux `script`) against the fake API in bench/mock.py, and
// the tests type at it and read what it draws.
//
// Every test has its own fake API, port, home, config, data and project
// folders, and never sees the real ANTHROPIC_API_KEY. The app is always left
// through /exit, ctrl-d, ctrl-c twice or a signal it handles, never killed.
//
// Needs: script (util-linux), stty, kill, touch, python3.

mod common;

mod commands;
mod prompts;
mod signals;
mod turn;
mod typing;
