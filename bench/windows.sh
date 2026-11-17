#!/usr/bin/env bash
# Checks the real binary on Windows against the fake API (no key, no network):
# a run with tools in Git Bash and in PowerShell, a session that continues,
# background tasks (start, merge, cancel) and the MCP server. Run it from Git
# Bash; CI does on every push.
#
#   bash bench/windows.sh target/x86_64-pc-windows-msvc/release/claudejr.exe
set -u
bin=$(cygpath -m "$(realpath "${1:?usage: bench/windows.sh CLAUDEJR.EXE}")")
root=$(cd "$(dirname "$0")/.." && pwd)
py=$(command -v python3 || command -v python)
work=$(mktemp -d)
mkdir -p "$work/cfg" "$work/data" "$work/proj"
export XDG_CONFIG_HOME=$(cygpath -m "$work/cfg") XDG_DATA_HOME=$(cygpath -m "$work/data")
export ANTHROPIC_API_KEY=mock CLAUDE_JR_OFFLINE_SECS=15
pass=0; fail=0
say() { if [ "$1" = ok ]; then pass=$((pass+1)); else fail=$((fail+1)); fi; printf '  %-4s %s\n' "$1" "$2"; }
field() { "$py" -c 'import sys,json; print(json.load(sys.stdin).get(sys.argv[1]))' "$1" 2>/dev/null | tr -d '\r'; }

# mock NAME=value...: a fake API with these settings; its address goes into the environment.
mock() {
    [ -n "${mock_pid:-}" ] && kill "$mock_pid" 2>/dev/null
    rm -f "$work/port" "$work/requests"*
    env MOCK_LOG="$(cygpath -m "$work/requests")" "$@" "$py" -I "$root/bench/mock.py" 0 >"$work/port" 2>/dev/null &
    mock_pid=$!
    for _ in $(seq 100); do [ -s "$work/port" ] && break; sleep 0.1; done
    export ANTHROPIC_BASE_URL="http://127.0.0.1:$(head -1 "$work/port" | tr -d '\r')"
}
trap '[ -n "${mock_pid:-}" ] && kill "$mock_pid" 2>/dev/null; rm -rf "$work"' EXIT

cd "$work/proj"
[ "$("$bin" --version | tr -d '\r' | cut -d' ' -f1)" = claude-jr ] && say ok "it starts" || say FAIL "--version"

mock
out=$("$bin" -y -p "do the demo" --output-format json 2>"$work/err")
[ "$(echo "$out" | field ok)" = True ] && [ "$(tr -d '\r' <demo.txt | tr '\n' ' ')" = "one TWO three " ] \
    && say ok "a run with tools: write, read, edit, and commands in Git Bash" || say FAIL "a run with tools: $out $(tail -2 "$work/err")"
grep -q 'xxxxxxxx' "$work/requests" && say ok "the command's output went back to the model" || say FAIL "no command output in the requests"
[ ! -s "$work/requests.invalid" ] && say ok "nothing the API would refuse was sent" || say FAIL "refused: $(cat "$work/requests.invalid")"

out=$("$bin" -y -c -p "carry on" --output-format json 2>"$work/err")
[ "$(echo "$out" | field ok)" = True ] && grep -q 'do the demo' <(tail -1 "$work/requests") \
    && say ok "the session continues with its history" || say FAIL "continuing: $(tail -2 "$work/err")"

mock MOCK_CMD='cd sub; pwd'
mkdir -p sub
"$bin" -y -p "go there" >/dev/null 2>"$work/err"
grep -q 'working directory is now' "$work/requests" && say ok "cd carries over, and the model is told" || say FAIL "cd: $(tail -2 "$work/err")"

mock MOCK_CMD='Write-Output ("from-" + "powershell"); Get-Location | Out-Null'
CLAUDE_JR_SHELL=powershell "$bin" -y -p "use powershell" >/dev/null 2>"$work/err"
grep -q 'from-powershell' "$work/requests" && say ok "commands run in PowerShell when that is the shell" || say FAIL "PowerShell: $(tail -2 "$work/err")"
grep -q 'the bash tool runs PowerShell' "$work/requests" && say ok "the model is told which shell it has" || say FAIL "the prompt doesn't name PowerShell"

# Background tasks need a repository to branch from.
git init -q -b main . && git config user.name t && git config user.email t@example.com && git config commit.gpgsign false
echo kept >kept.txt && git add -A && git commit -q -m first
mock
id=$("$bin" task start -y "add the demo file" | tr -d '\r')
[ -n "$id" ] && say ok "a task starts in the background ($id)" || say FAIL "task start"
state=$("$bin" task wait "$id" 2>"$work/err")
[ "$(echo "$state" | field status)" = done ] && say ok "it finishes, and waiting for it sees that" || say FAIL "task wait: $state $(tail -2 "$work/err")"
"$bin" task list | grep -q "claude-haiku-5-5" && say ok "the list names the model that worked" || say FAIL "task list: $("$bin" task list)"
"$bin" task merge "$id" >/dev/null 2>"$work/err" && git log --oneline | grep -q . && [ -f demo.txt ] \
    && say ok "its branch merges into this one" || say FAIL "task merge: $(tail -2 "$work/err")"

mock MOCK_CMD='sleep 60' MOCK_ROUNDS=1
id=$("$bin" task start -y "slow work" | tr -d '\r')
sleep 3
[ "$("$bin" task result "$id" | field status)" = running ] && say ok "a running task is seen as running" || say FAIL "not running: $("$bin" task result "$id")"
"$bin" task cancel "$id" >/dev/null 2>&1
status=$("$bin" task result "$id" | field status)
[ "$status" = cancelled ] && say ok "cancelling stops it" || say FAIL "after cancel: $status"

reply=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}' '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' | "$bin" mcp -y 2>/dev/null)
echo "$reply" | grep -q '"name":"claude-jr"' && echo "$reply" | grep -q start_task \
    && say ok "the MCP server answers" || say FAIL "mcp: ${reply:0:200}"

echo "windows: $pass passed, $fail failed"
[ "$fail" = 0 ]
