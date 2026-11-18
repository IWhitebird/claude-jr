# How claude-jr works

Plain threads, a conversation kept on disk, and nothing else running.

## Threads

Four threads when idle, all asleep in the kernel, which is why waiting costs no CPU.

```mermaid
flowchart LR
  keys["keys thread"] -- "key presses" --> ui["UI thread"]
  ui -- "commands" --> agent["agent thread"]
  agent -- "events" --> ui
  agent <-- "one streamed connection" --> api(["the model's API"])
  agent -- "tool calls" --> tools["tools, subagents,<br/>commands"]
```

There is no async runtime and nothing polls: an idle session uses no CPU.

## One turn

```mermaid
flowchart LR
  t_you(["you type"]) --> t_agent["agent"]
  t_agent -- "request" --> t_api(["the model"])
  t_api -- "streamed reply" --> t_more{"tool calls?"}
  t_more -- "yes: permit, run,<br/>append the results" --> t_agent
  t_more -- "no" --> t_done(["done"])
```

Reads, subagents, skills and MCP calls in one reply run in parallel; edits and
commands run in order, each after the permission check.

## Where the conversation lives

```mermaid
flowchart LR
  d_memory["in memory<br/>where each message is,<br/>plus the newest one"]
  d_file["the session file, on disk<br/>every saved message, one per line"]
  d_memory -. "points into" .-> d_file
  d_file == "read as the request is sent" ==> d_api(["the request"])
  d_memory == "the newest message" ==> d_api
```

- **On disk, not in memory.** A saved message is a line in the session file; memory keeps where it is. A long session uses the same memory as a short one.
- **Stored as it is sent.** A message is the exact JSON the API gets, so a request is streamed straight from the file.
- **Append-only.** Nothing already sent is edited, which keeps the provider's prompt cache valid.
