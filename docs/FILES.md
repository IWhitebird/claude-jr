# Where everything is

Every source file and what it holds, for people working on the code. How it works, in pictures, is in [ARCHITECTURE.md](ARCHITECTURE.md).

| Folder | What is in it |
|---|---|
| `src/cli` | the command line, one file per thing you can type |
| `src/ui` | the interactive terminal |
| `src/agent` | the turn loop, permissions, subagents |
| `src/tools` | what the model can do: files, commands, MCP, skills |
| `src/api` | talking to models: each wire format and provider |
| `src/work` | background tasks, worktrees, budgets, the run log |
| `src/store` | sessions, images and config on disk |
| `src/sys` | what differs between Linux, macOS and Windows |

```
src/
  main.rs            parse the command line, hand over to cli/
  cli/               one file for each thing you can type
    args.rs          the flags and the usage text
    start.rs         claudejr and claudejr -p: start-up checks, then the UI or print
    print.rs         -p: one prompt, the reply printed (text, JSON, or a stream of steps)
    sessions.rs      --sessions
    update.rs        --update
    task.rs          claudejr task ...
    fleet.rs         claudejr fleet ...
    mcp.rs           claudejr mcp: tasks served to another agent
    install.rs       claudejr install / uninstall
  ui/                the interactive terminal
    mod.rs           the event loop
    app.rs           the state, the scrollback, the live area
    events.rs        what the agent reports
    keys.rs          key presses, prompts, settings
    commands.rs      what is typed and sent, slash commands
    term.rs          raw mode, scrollback, line-diff redraw
    history.rs       what was drawn, kept to draw again at a new width
    view.rs          tool blocks, diffs, boxes, status, footer, mascot
    md.rs  table.rs  text.rs   Markdown, tables, styles and wrapping
    input.rs  paste.rs         the line editor, pasted text
    stats.rs         /stats
  agent/             the loop
    mod.rs           the agent thread: start-up, commands, sending a message
    turn.rs          one turn: ask, run tools, repeat (shared with subagents)
    permit.rs        modes, approval, auto mode's reviewer
    sub.rs           subagents, in worktrees of their own when asked
    compact.rs       /compact
    history.rs       rewind, resume, the transcript
  tools/             what the model can do
    mod.rs           definitions, dispatch, shared state behind locks
    fs.rs            read, write, edit
    bash.rs          bash, background jobs, the jobs tool
    plain.rs         command output as a terminal shows it: no colour codes, no overwritten progress
    mcp.rs           MCP servers over stdio, and the API's connector for remote ones
    skill.rs  memory.rs  todo.rs  checkpoint.rs
  api/               talking to models
    mod.rs           the client: where a request goes, retries, cancelling
    body.rs          the request body, streamed from memory, files and images
    stream.rs        reading an event stream line by line
    anthropic.rs     the Messages format
    openai.rs        the Chat Completions format, translated both ways
    provider.rs      which provider a model id names
    cloud.rs         Claude through Amazon Bedrock or Google Vertex AI
    bedrock.rs       Bedrock's address, login and request signing
    vertex.rs        Vertex's address and access token
    models.rs        prices, context sizes, what each Claude model supports
  work/              running many
    task.rs          background tasks: start, state, wait, cancel, merge, drop
    budget.rs        a group's shared budget
    worktree.rs      a git worktree and branch for a run
    log.rs           the run log: every step as a JSON line
  store/             what is kept on disk
    session.rs       conversations as JSONL, read back in place
    images.rs        pasted and attached images
    config.rs        config files, the saved API key, trusted folders
  sys/               what depends on the operating system
    os.rs            files and processes, on Unix and on Windows
    shell.rs         the shell commands run in (bash; on Windows Git's, or PowerShell)
    sandbox.rs       re-exec inside bubblewrap (Linux)
    mem.rs           handing unused program pages back (Linux)
    signals.rs       clean exit on signals, or on Windows when the console closes
  permission.rs      allow rules
  prompt.rs          the system prompt, AGENTS.md, notes

tests/
  cli/               the real binary against the fake API, a file per area
  ui/                the real binary in a pseudo-terminal

bench/               measurements and checks; see bench/README.md

build.sh             the release build
install.sh           the installer for Linux and macOS; install.ps1 for Windows
Dockerfile           the Docker image: the binary on busybox
Formula/claude-jr.rb the Homebrew formula, written by scripts/formula.sh at each release
scripts/demo.py      records the session shown in the README and on the site (assets/demo.svg, site/demo.svg)
```

Files stay under about 500 lines of code. Where a file's tests are longer
than the code, they sit beside it (`api/tests.rs`, `agent/tests.rs`).
