<p align="center">
  <img src="assets/logo-animated.svg" width="140" alt="claude-jr (Claude Junior) mascot">
</p>

<h1 align="center">claude-jr</h1>

<p align="center">
  <b>Claude Junior</b>: a small, fast alternative to Claude Code.<br>
  A full coding agent for the terminal, in one 2 MB binary.
</p>

<p align="center">
  <a href="https://github.com/IWhitebird/claude-jr/actions/workflows/ci.yml"><img src="https://github.com/IWhitebird/claude-jr/actions/workflows/ci.yml/badge.svg" alt="tests"></a>
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT license">
</p>

<table align="center">
  <tr>
    <td align="center"><b>2 MB</b><br>to install<br><sub>Claude Code: 354 MB</sub></td>
    <td align="center"><b>0.7 MB</b><br>of memory, idle<br><sub>Claude Code: 148 MB</sub></td>
    <td align="center"><b>4 ms</b><br>to start<br><sub>Claude Code: 157 ms</sub></td>
    <td align="center"><b>93.4%</b><br>SWE-bench Verified<br><sub>467 of 500, Opus 5.5</sub></td>
  </tr>
</table>

<p align="center">
  <img src="assets/demo.svg" width="760" alt="A claude-jr session in a terminal. Asked to fix failing tests, it runs them, edits one line, runs them again and reports; then /stats shows it holding 1.6 MB of memory.">
  <br>
  <sub>A real session with Claude Opus 5.5, in real time. <code>/stats</code> at the end: 1.6 MB of memory.</sub>
</p>

## Install

Linux and macOS:

```sh
curl -fsSL https://claudejr.iwhitebird.com/install | sh
```

Windows, in PowerShell:

```powershell
irm https://claudejr.iwhitebird.com/install.ps1 | iex
```

Then run `claudejr`. It asks for an Anthropic API key the first time; to use
another model instead, `claudejr -m ollama/qwen3`.

Also on [Homebrew, Cargo and Docker](docs/GUIDE.md#other-ways-to-install).

## What you get

- **Any model.** Claude by default; OpenAI, Gemini, Groq, OpenRouter or a local Ollama with `-m provider/model`.
- **It asks first.** Five permission modes, a sandbox, and `/rewind` to take back anything it did.
- **Many at once.** Background tasks on their own git branches, and Claude Code, Cursor or Codex can hand it work over MCP. A thousand agents fit in 500 MB.
- **The usual kit.** Subagents, skills, MCP servers, images, web search, sessions you can resume.
- **Tested.** 423 tests, 97% line coverage, and random-input runs against the real app in a terminal.

## Use

```sh
claudejr                                      # start in this folder
claudejr -c                                   # continue the last conversation
claudejr -p "fix the failing test" -y         # one task, no questions: for scripts and CI
claudejr -m ollama/qwen3                      # another provider's model
claudejr task start -y "fix the flaky test"   # in the background, on its own branch
```

Inside it, `/` lists every command, `shift-tab` changes how much it asks, and
`esc` stops it.

## More

[Guide](docs/GUIDE.md) · [Benchmarks](docs/BENCHMARKS.md) · [How it works](docs/ARCHITECTURE.md) · [Website](https://claudejr.iwhitebird.com)

The comparison is with Claude Code 2.1.326 on the same laptop, 18 November
2026. Claude Code does a good deal more than claude-jr; this is what the
difference costs. The benchmarks page has every number and how it was
measured, including the ones that didn't go our way.

MIT licence.
