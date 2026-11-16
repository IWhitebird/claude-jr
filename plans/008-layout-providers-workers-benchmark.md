# 008: A readable layout, Bedrock and Vertex, cheap workers, the full SWE-bench

Written 2026-11-17 against commit `f46b642` (0.2.0). Decisions settled with
the user the same day (see the end). Nothing here is started.

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

Every commit is pushed as it lands, provided the full suite passes first.
A release is cut only when the user asks for one; the natural point is after
phase 4, as 0.3.0.

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
format and the same event stream, behind their own address and login. So each
is a thin layer in `api/`, and the conversation, tools and everything above
the client stay untouched.

Step 2.0 is done: the tables below are what Anthropic's documentation said on
2026-11-17 ("Claude in Amazon Bedrock", "Claude Models on Google Cloud
(Google managed)", "Beta headers"). The first draft of this plan had Bedrock
wrong: it described the older `bedrock-runtime` address, which wraps the
stream in AWS's binary frames. The current one (`bedrock-mantle`) is the
plain Messages API, so no frame reader is needed.

### Bedrock

| | |
|---|---|
| Used as | `claudejr -m bedrock/claude-opus-5-5` (sent as `anthropic.claude-opus-5-5`; an id that already has the prefix is sent as it is) |
| Address | `https://bedrock-mantle.REGION.api.aws/anthropic/v1/messages` |
| Region | `AWS_REGION`, then `AWS_DEFAULT_REGION`, else `us-east-1` |
| Login, simplest | a Bedrock API key in `AWS_BEARER_TOKEN_BEDROCK`, sent in `x-api-key`; it wins when both are set |
| Login, standard | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and optionally `AWS_SESSION_TOKEN`, signing each request (AWS Signature V4, service `bedrock-mantle`) |
| Request | Anthropic's, unchanged, with the `anthropic-version` and `anthropic-beta` headers |
| Reply | Anthropic's ordinary event stream |
| Left out | the refusal fallback, web search and fetch, remote MCP servers: Bedrock has none of them |
| Not served | Fable 5.2 (Bedrock has it only at the older address, which this version does not speak) |

Work:

- **Signing.** About 100 lines using the SHA-256 and HMAC already in the
  binary (`ring`, which the TLS library brings in; it becomes a direct
  dependency, not a new one). The signature covers the body's hash, and the
  body is streamed from disk, so the hash is computed in a first pass over
  the same parts. That is one extra read of the history per request; its CPU
  cost is measured against the budget. A Bedrock API key needs none of this.
- **Not in this version:** reading `~/.aws` profiles, SSO, or instance roles.
  The error for missing credentials says how to export them into the
  environment.

### Vertex

| | |
|---|---|
| Used as | `claudejr -m vertex/claude-opus-5-5` (older models with their date: `vertex/claude-haiku-4-5@20251001`) |
| Address | `https://HOST/v1/projects/PROJECT/locations/REGION/publishers/anthropic/models/MODEL:streamRawPredict` |
| Host | `aiplatform.googleapis.com` for `global`; `aiplatform.us.rep.googleapis.com` for `us` and `eu`; `REGION-aiplatform.googleapis.com` for a single region |
| Project | `ANTHROPIC_VERTEX_PROJECT_ID`, then `GOOGLE_CLOUD_PROJECT` |
| Region | `CLOUD_ML_REGION`, else `global` |
| Login | `VERTEX_ACCESS_TOKEN` if set; otherwise the output of `gcloud auth print-access-token`, kept for 45 minutes; sent as a bearer token |
| Request | Anthropic's, with `anthropic_version` in the body, no `model` in the body, and the `anthropic-beta` header |
| Reply | Anthropic's ordinary event stream |
| Left out | the refusal fallback, web fetch, remote MCP servers (Vertex has none of them) and web search (Vertex has only the older version of it) |

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
- Both are marked in the guide, the README and `--help` as **not yet tested
  against the real service**: there is no AWS account or Google Cloud project
  to test with for now. What stands in for that:
  - the fake API checks every request the way the real services document it,
    including that a Bedrock signature is right for its body;
  - the signing code is checked against AWS's published test vectors;
  - `bench/provider.sh bedrock/...` and `vertex/...` are ready, so the real
    test is one command for whoever has an account.
- The label comes off only after a real run of `bench/provider.sh` passes.

### Expected cost

Binary about +20 KB for Bedrock (no frame reader after all) and +15 KB for Vertex. Idle memory and idle
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

## Order of work

| # | Step | Ends with |
|---|---|---|
| 1 | Phase 1, steps 1.1 to 1.8, one commit each | the suite green and pushed after every step; the binary within a few hundred bytes of 2,013,920 |
| 2 | Step 2.0: read the current Bedrock and Vertex documentation | the two tables in phase 2 corrected where they are wrong |
| 3 | Vertex (the smaller one, and it exercises the new `api/` layout first) | tests, budgets measured |
| 4 | Bedrock: API-key login, then signing | tests including AWS's signing vectors, budgets measured, CPU reported |
| 5 | Phase 3: the worker model | tests, guide |
| 6 | Phase 4: SWE-bench overnight, then the worker benchmark | the full-set figure and the comparison table published |
| 7 | Stop and report; release 0.3.0 when asked | |

After each numbered step: the budgets table re-measured and reported, with
anything that moved.

## Decided (2026-11-17)

| Question | Decision |
|---|---|
| Reorganise before any feature work? | Yes |
| The default model for workers | `claude-haiku-5-5` |
| Accounts to test Bedrock and Vertex against the real services | None for now; both ship labelled untested, to be tested later |
| Spend about $20 on SWE-bench and $5 to $15 on the worker benchmark | Yes |
| Releases | Work is pushed continuously; a release when asked |
