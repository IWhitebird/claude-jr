#!/usr/bin/env bash
# Checks the real binary on Windows against the fake API (no key, no network):
# a run with tools in Git Bash and in PowerShell, a session that continues,
# background tasks (start, merge, cancel), the MCP server, the installer and
# --update against a release folder of its own, and how much memory a run
# takes. Run it from Git Bash; CI does on every push.
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
# What a task left behind, for when a check on it fails.
show() {
    echo "--- task $1: result, then the end of its output and log"
    "$bin" task result "$1" 2>&1 | head -40
    tail -15 "$work/data/claude-jr/tasks/$1/output.txt" 2>/dev/null
    tail -8 "$work/data/claude-jr/tasks/$1/log.jsonl" 2>/dev/null | cut -c1-300
    git worktree list 2>&1; git branch -a 2>&1
    echo "---"
}
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
trap '[ -n "${mock_pid:-}" ] && kill "$mock_pid" 2>/dev/null; [ -n "${web_pid:-}" ] && kill "$web_pid" 2>/dev/null; rm -rf "$work"' EXIT

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
mkdir "$work/repo" && cd "$work/repo"
git init -q -b main . && git config user.name t && git config user.email t@example.com && git config commit.gpgsign false
echo kept >kept.txt && git add -A && git commit -q -m first
mock
id=$("$bin" task start -y "add the demo file" | tr -d '\r')
[ -n "$id" ] && say ok "a task starts in the background ($id)" || say FAIL "task start"
state=$("$bin" task wait "$id" 2>"$work/err")
[ "$(echo "$state" | field status)" = done ] && say ok "it finishes, and waiting for it sees that" || say FAIL "task wait: $state $(tail -2 "$work/err")"
"$bin" task list | grep -q "claude-haiku-5-5" && say ok "the list names the model that worked" || say FAIL "task list: $("$bin" task list)"
"$bin" task merge "$id" >/dev/null 2>"$work/err" && git log --oneline | grep -q . && [ -f demo.txt ] \
    && say ok "its branch merges into this one" || { say FAIL "task merge: $(tail -2 "$work/err")"; show "$id"; }

mock MOCK_CMD='sleep 60' MOCK_ROUNDS=1
# (Other steps of a CI job can have left a sleep of their own running.)
sleeping() { tasklist 2>/dev/null | grep -ci '^sleep'; }
before=$(sleeping)
began=$(date +%s)
id=$("$bin" task start -y "slow work" | tr -d '\r')
[ $(( $(date +%s) - began )) -lt 10 ] && say ok "starting returns at once, to a script that captures the id too" || say FAIL "task start waited for its worker"
sleep 3
[ "$("$bin" task result "$id" | field status)" = running ] && say ok "a running task is seen as running" || { say FAIL "not running"; show "$id"; }
"$bin" task cancel "$id" >/dev/null 2>&1
status=$("$bin" task result "$id" | field status)
[ "$status" = cancelled ] && say ok "cancelling stops it" || { say FAIL "after cancel: $status"; show "$id"; }
sleep 3
after=$(sleeping)
[ "$after" -le "$before" ] && say ok "and the command it was running" \
    || { say FAIL "the cancelled task's command is still running ($before sleeps before, $after after)"; show "$id"; }

reply=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}' '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' | "$bin" mcp -y 2>/dev/null)
echo "$reply" | grep -q '"name":"claude-jr"' && echo "$reply" | grep -q start_task \
    && say ok "the MCP server answers" || say FAIL "mcp: ${reply:0:200}"

# ---- installing and updating, from a release folder served here ----------
cd "$work"
version=$("$bin" --version | tr -d '\r' | cut -d' ' -f2)
mkdir rel && cp "$bin" rel/claudejr-windows-x86_64.exe && echo "$version" >rel/version
# (As the release makes it, on Linux: two spaces, no binary-mode star.)
(cd rel && sha256sum claudejr-windows-x86_64.exe | sed 's/ \*/  /' >checksums.txt)
serve() { # serve DIR: a web server for it; its address goes into CLAUDE_JR_RELEASES
    [ -n "${web_pid:-}" ] && kill "$web_pid" 2>/dev/null
    port=$("$py" -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
    "$py" -m http.server "$port" --bind 127.0.0.1 --directory "$(cygpath -m "$1")" >/dev/null 2>&1 &
    web_pid=$!
    sleep 1.5
    export CLAUDE_JR_RELEASES="http://127.0.0.1:$port"
}
installer=$(cygpath -m "$root/install.ps1")
serve "$work/rel"
CLAUDE_JR_BIN=$(cygpath -w "$work/bin") powershell -NoProfile -ExecutionPolicy Bypass -File "$installer" >"$work/install.out" 2>&1
[ "$("$work/bin/claudejr.exe" --version 2>/dev/null | tr -d '\r')" = "claude-jr $version" ] \
    && say ok "install.ps1 installs the release and it runs" || say FAIL "install.ps1: $(tail -3 "$work/install.out")"
# Over a copy that is already there (as when updating by installing again).
CLAUDE_JR_BIN=$(cygpath -w "$work/bin") powershell -NoProfile -ExecutionPolicy Bypass -File "$installer" >"$work/install.out" 2>&1 \
    && [ -f "$work/bin/claudejr.exe" ] && say ok "installing again replaces the one that is there" || say FAIL "installing again: $(tail -3 "$work/install.out")"
# A download that doesn't match its checksum is not installed.
mkdir bad && cp rel/* bad/ && echo tampered >>bad/claudejr-windows-x86_64.exe
serve "$work/bad"
CLAUDE_JR_BIN=$(cygpath -w "$work/bin2") powershell -NoProfile -ExecutionPolicy Bypass -File "$installer" >"$work/install.out" 2>&1
[ ! -e "$work/bin2/claudejr.exe" ] && grep -q "checksum" "$work/install.out" \
    && say ok "a download that fails its checksum is refused" || say FAIL "a bad checksum got through: $(tail -3 "$work/install.out")"

# --update: nothing newer, then a newer release (a stand-in that says 9.9.9).
serve "$work/rel"
"$work/bin/claudejr.exe" --update 2>&1 | grep -q "is the newest version" && say ok "--update with nothing newer changes nothing" || say FAIL "--update, nothing newer"
if command -v rustc >/dev/null; then
    mkdir new && echo 'fn main() { println!("claude-jr 9.9.9"); }' >new/v.rs
    rustc -o "$(cygpath -m "$work/new/claudejr-windows-x86_64.exe")" "$(cygpath -m "$work/new/v.rs")" 2>/dev/null
    echo 9.9.9 >new/version
    serve "$work/new"
    out=$("$work/bin/claudejr.exe" --update 2>&1)
    [ "$("$work/bin/claudejr.exe" --version | tr -d '\r')" = "claude-jr 9.9.9" ] && echo "$out" | grep -q "Updated claude-jr $version to 9.9.9" \
        && say ok "--update replaces the program while it runs" || say FAIL "--update: $out"
    # And once more: the copy set aside last time must not be in the way.
    echo 'fn main() { println!("claude-jr 9.9.10"); }' >new/v.rs
    rustc -o "$(cygpath -m "$work/new/next.exe")" "$(cygpath -m "$work/new/v.rs")" 2>/dev/null
    cp "$bin" "$work/bin/claudejr.exe" 2>/dev/null || { mv "$work/bin/claudejr.exe" "$work/bin/x.exe"; cp "$bin" "$work/bin/claudejr.exe"; }
    cp new/next.exe new/claudejr-windows-x86_64.exe && echo 9.9.10 >new/version
    "$work/bin/claudejr.exe" --update >/dev/null 2>&1
    [ "$("$work/bin/claudejr.exe" --version | tr -d '\r')" = "claude-jr 9.9.10" ] && say ok "and again, over what the last update left" || say FAIL "a second --update"
fi
kill "$web_pid" 2>/dev/null

# ---- memory: what a run takes on Windows (reported, not judged) ----------
cd "$work/proj"
mock MOCK_ROUNDS=20 MOCK_OUTPUT_KB=40
peak=$(powershell -NoProfile -Command "\$p = Start-Process -FilePath '$bin' -ArgumentList '-y','-p','run' -PassThru -WindowStyle Hidden; \$peak = 0; while (-not \$p.HasExited) { \$p.Refresh(); if (\$p.PeakWorkingSet64 -gt \$peak) { \$peak = \$p.PeakWorkingSet64 }; Start-Sleep -Milliseconds 20 }; [math]::Round(\$peak / 1MB, 1)" | tr -d '\r')
echo "  memory: a run of 20 commands peaked at ${peak} MB (working set)"
idle=$(powershell -NoProfile -Command "\$i = New-Object System.Diagnostics.ProcessStartInfo; \$i.FileName = '$bin'; \$i.Arguments = 'mcp -y'; \$i.RedirectStandardInput = \$true; \$i.RedirectStandardOutput = \$true; \$i.UseShellExecute = \$false; \$p = [System.Diagnostics.Process]::Start(\$i); Start-Sleep 3; \$p.Refresh(); [math]::Round(\$p.WorkingSet64 / 1MB, 1), [math]::Round(\$p.PrivateMemorySize64 / 1MB, 1) -join ' '; \$p.Kill()" | tr -d '\r')
echo "  memory: the MCP server waiting for calls holds (working set, private) ${idle} MB"

echo "windows: $pass passed, $fail failed"
[ "$fail" = 0 ]
