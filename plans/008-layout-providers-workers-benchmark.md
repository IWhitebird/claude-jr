# 008: A readable layout, Bedrock and Vertex, cheap workers, the full SWE-bench

Written 2026-11-17 against commit `f46b642` (0.2.0). Nothing here is started.

## What this is for

0.2.0 made claude-jr the worker other agents hand tasks to. This plan does
four things on top of it, in this order:

1. Reorganise the source so a newcomer can find things (no behaviour change).
2. Add the two remaining ways companies buy Claude: AWS Bedrock and Google Vertex.
3. Make workers cheap by default.
4. Finish the SWE-bench run and publish the full-set number.

Launch material (a recording, the posts, the link-preview image) follows once
3 and 4 have produced the numbers; it is listed at the end and not planned in
detail here. Native Windows is on hold.

## Budgets: what must not get worse

Measured on the released 0.2.0 Linux x86_64 binary. Each phase ends by
re-measuring these; a phase that misses one is not done.

| | 0.2.0 | Allowed after this plan |
|---|---|---|
| Binary | 2.01 MB | 2.10 MB |
| Memory, idle | 0.7 MB | 0.7 MB |
| Memory, working (20 tool calls) | 2.1 MB | 2.2 MB |
| Peak, 200 tool calls | 3.9 MB | 4.0 MB |
| CPU, idle 10 s | 0 | 0 |
| CPU, 200 tool calls (Anthropic) | 0.40 s | 0.42 s |
| 1,000 agents at once | 488 MB | 500 MB |

Phase 1 moves code without changing it, so its bar is stricter: the binary
may differ by at most a few hundred bytes and every other row must be equal.

## Phase 1: a layout that reads well

### What is wrong today

`src/` has 14 files at the top level that grew one feature at a time, and
five files carry too much:

| File | Lines of code (without tests) | What is in it |
|---|---|---|
| `ui/mod.rs` | 1,498 | the event loop, app state, key handling, slash commands, agent events, drawing |
| `api.rs` | 871 | the client, retries, the request body, Anthropic's stream, model prices and capabilities |
| `task.rs` | 699 | the task registry, budgets, the fleet, and the command line for both |
| `agent/mod.rs` | 620 | start-up, the command loop, compaction, rewind, resume, the transcript |
| `main.rs` | 332 | argument parsing, start-up checks, and dispatch for seven subcommands |

`tests/cli.rs` is 2,250 lines covering eight unrelated areas.

### The target

```
src/
  main.rs            only: parse, dispatch, exit code
  cli/               one file per thing you can type
    args.rs          flags and usage text
    start.rs         start-up checks shared by the UI and -p (key, trust, sandbox, worktree)
    print.rs         -p
    task.rs          claudejr task ...
    fleet.rs         claudejr fleet ...
    mcp.rs           claudejr mcp   (the server, from serve.rs)
    install.rs       claudejr install / uninstall
    update.rs        claudejr --update
  api/               talking to models
    mod.rs           Client: where a request goes, retries, cancelling
    body.rs          the request body streamed from memory, files and images
    stream.rs        reading an event stream line by line, timeouts
    anthropic.rs     the Messages format: request head, reply parsing
    openai.rs        the Chat Completions format: translation both ways
    bedrock.rs       (phase 2)
    vertex.rs        (phase 2)
    provider.rs      which provider a model id names
    models.rs        prices, context sizes, what each Claude model supports
  agent/             the loop
    mod.rs           start-up and the command loop
    turn.rs          one turn: ask, run tools, repeat
    permit.rs        may this call run
    sub.rs           subagents
    compact.rs       /compact
    history.rs       rewind, resume, the transcript shown on resume
  tools/             unchanged: bash, fs, mcp, memory, skill, todo, checkpoint
  work/              running many
    task.rs          a task's folder, state, start, wait, cancel, merge, drop
    budget.rs        a group's shared budget
    worktree.rs
    log.rs           the run log
  store/             what is kept on disk
    session.rs
    images.rs
    config.rs
  ui/                the terminal
    mod.rs           the event loop only
    app.rs           the state and what it shows
    keys.rs          key presses
    commands.rs      slash commands
    events.rs        what the agent reports
    (history, input, md, paste, stats, table, term, text, view: unchanged)
  sys/               what differs per operating system
    sandbox.rs  mem.rs  signals.rs
  permission.rs  prompt.rs

tests/
  cli/main.rs        one test binary, as now
    common.rs        Env, the fake API, helpers
    args.rs  print.rs  providers.rs  failures.rs  worktree.rs
    tasks.rs  fleet.rs  subagents.rs  mcp.rs  install.rs  sandbox.rs  update.rs
  ui/main.rs
    common.rs        World, App
    (the tests, grouped as the section comments in ui.rs already group them)

docs/
  ARCHITECTURE.md    moved from the top level, redrawn for the new layout
  GUIDE.md  BENCHMARKS.md
bench/README.md      one table: each script, what it measures, how to run it
```

`sys/` is also where a Windows port would later put its own versions, which
is the main reason to gather those three files now.

### Rules the code follows afterwards

- No file over about 500 lines of code. Tests don't count.
- A file's unit tests stay in the file. Where they are longer than the code
  (`api`, `agent`, `openai`), they move to a sibling `tests.rs` so the code
  can be read without scrolling past them.
- Every file opens with a comment saying what it is and how it fits, as most
  already do.
- `mod.rs` holds what the folder's files share and nothing else.
- Things used only inside a folder are not `pub`.
- `PLAN.md` (the original build plan, long finished) and the seven completed
  plans in `plans/` move to `docs/history/`.

### How it is done

Moves only. No renamed functions, no changed logic, no new features, so a
reviewer can check each commit by seeing that lines moved and nothing else did.

One commit per row, each with the full suite green before the next starts:

| Step | Move |
|---|---|
| 1.1 | `api.rs` into `api/` (mod, body, stream, anthropic, models); `openai.rs` and `provider.rs` join it |
| 1.2 | `task.rs`, `worktree.rs`, `log.rs` into `work/`; budget split out |
| 1.3 | `main.rs`, `print.rs`, `serve.rs`, `install.rs`, `update.rs` and the task and fleet command lines into `cli/` |
| 1.4 | `session.rs`, `images.rs`, `config.rs` into `store/`; `sandbox.rs`, `mem.rs`, `signals.rs` into `sys/` |
| 1.5 | `ui/mod.rs` into mod, app, keys, commands, events |
| 1.6 | `agent/mod.rs` into mod, compact, history |
| 1.7 | `tests/cli.rs` and `tests/ui.rs` into folders with a shared `common.rs` |
| 1.8 | Docs: ARCHITECTURE redrawn and moved, `bench/README.md`, old plans to `docs/history/`, AGENTS.md layout section updated |

### Done when

- All 369 tests pass, also pinned to one core, with the same test names.
- `cargo clippy` and `cargo fmt --check` are clean.
- The release binary's size is within a few hundred bytes of 2,013,920.
- The resize checker (26 cases), the chaos run and the feature check pass.
- No file in `src/` exceeds about 500 lines of code.

### Risk

Low, but wide: every file is touched, so anything else in progress would
conflict. Nothing else starts until phase 1 has landed.

## Phase 2: Bedrock and Vertex

Both sell the same Claude models as Anthropic's API, with the same message
format, behind their own address, login and (for Bedrock) stream framing. So
each is a thin layer in `api/`, and the conversation, tools and everything
above the client stay untouched.

Step 2.0 comes first for both: read the current official documentation for
the exact paths, version strings, model id forms and which request fields
each accepts. What follows is my understanding and has to be confirmed there
before any code is written.

### Bedrock

| | |
|---|---|
| Used as | `claudejr -m bedrock/MODEL_ID` |
| Region | `AWS_REGION`, then `AWS_DEFAULT_REGION`, else `us-east-1` |
| Login, simplest | a Bedrock API key in `AWS_BEARER_TOKEN_BEDROCK`, sent as a bearer token |
| Login, standard | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and optionally `AWS_SESSION_TOKEN`, signing each request (AWS Signature V4) |
| Request | Anthropic's format with a Bedrock version field, and the model in the address instead of the body |
| Reply | AWS's binary event framing, each frame holding one of Anthropic's stream events, base64-encoded |

Work:

- **Signing.** About 150 lines using the SHA-256 and HMAC already in the
  binary (`ring`, which the TLS library brings in; it becomes a direct
  dependency, not a new one). The signature covers the body's hash, and the
  body is streamed from disk, so the hash is computed in a first pass over
  the same parts. That is one extra read of the history per request; its CPU
  cost is measured against the budget.
- **Frame reader.** About 120 lines: length-prefixed frames, headers skipped,
  the payload decoded and fed to the existing Anthropic reply parser.
- **Not in this version:** reading `~/.aws` profiles, SSO, or instance roles.
  The error for missing credentials says how to export them into the
  environment.

### Vertex

| | |
|---|---|
| Used as | `claudejr -m vertex/MODEL` |
| Project | `ANTHROPIC_VERTEX_PROJECT_ID`, then `GOOGLE_CLOUD_PROJECT` |
| Region | `CLOUD_ML_REGION`, else `global` |
| Login | `VERTEX_ACCESS_TOKEN` if set; otherwise the output of `gcloud auth print-access-token`, kept until shortly before it expires |
| Request | Anthropic's format with a Vertex version field, and the model in the address |
| Reply | Anthropic's ordinary event stream |

Work: about 100 lines, mostly building the address and getting the token.
Service-account key files (which need a signed token) are left for later.

### For both

- Claude's own model names carry over: prices, context sizes and capabilities
  are looked up by the model, whichever door it came through.
- The fake API learns both doors, with the same strict checks, including a
  check that a Bedrock request's signature is right for its body.
- The failure matrix (ten kinds of failure at three points, then continuing
  the session) runs through both, as it does for the two formats today.
- `bench/provider.sh` works with `bedrock/...` and `vertex/...`.

### Done when

- The new tests pass and the budgets hold, with CPU for a 200-call session
  through Bedrock measured and reported beside Anthropic's.
- **Tested against the real services, or labelled as not.** This needs an AWS
  account and a Google Cloud project with Claude enabled. Without them both
  ship marked "untested against the real service" in the guide.

### Expected cost

Binary about +50 KB for Bedrock and +15 KB for Vertex. Idle memory and idle
CPU unchanged: none of this code runs or is loaded while waiting.

## Phase 3: workers that are cheap by default

Handing work to claude-jr only makes sense if it costs less than the main
agent doing it. Today a worker uses the same default model as an interactive
session (Opus).

- A `worker_model` setting, used by `task start`, `fleet`, `mcp` and
  subagents when no model is named.
- Its default: `claude-haiku-5-5` when the main model is Anthropic's; the
  main model itself for any other provider.
- The model each task used appears in `task list`, the fleet's table and
  `task_result`, so nobody is surprised by what did the work.
- `claudejr install` says which model the workers will use and how to change it.

Done when the tests cover each of the four places and the guide explains it.
No effect on size, memory or CPU.

## Phase 4: the full SWE-bench

- 347 of 500 tasks are done (326 resolved, 93.9%). 153 remain, about $20.
- Run overnight as before: a user service with a memory cap, three at a time.
- The run uses whatever binary is current, so it starts after phases 1 to 3,
  and the earlier 347 results stay as they are (same prompts and tools).
- Publish the full-set figure in the README, the site and BENCHMARKS, with
  the sample-size wording replaced by "all 500".
- Add the benchmark for the new pitch at the same time: ten independent
  tasks done by one agent alone, by one agent with its own subagents, and by
  one agent handing them to claude-jr workers. Wall time, memory, cost.
  About $5 to $15.

## After that

- Launch: a 30-second recording, posts for r/ClaudeAI, r/LocalLLaMA and
  Hacker News, a new link-preview image.
- Package managers: Homebrew, `cargo install`, a Docker image.
- Cursor and Codex checked against the real programs.
- Native Windows, when users ask.

## Decisions needed before starting

| # | Question | My suggestion |
|---|---|---|
| 1 | Phase 1 first, before any feature work? | Yes: Bedrock and Vertex then land in `api/` cleanly |
| 2 | Is Haiku the right default for workers? | Yes; Sonnet is the alternative if quality matters more than cost |
| 3 | Can you provide an AWS account and a Google Cloud project with Claude enabled, for testing? | If not, both ship labelled untested |
| 4 | Spend about $20 on SWE-bench and $5 to $15 on the worker benchmark? | Yes to both; they are the launch's evidence |
| 5 | Release phase 1 on its own (0.2.1) or with the rest (0.3.0)? | With the rest: phase 1 changes nothing a user can see |
