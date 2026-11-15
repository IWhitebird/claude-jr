#!/usr/bin/env bash
# Checks claude-jr against a real model of any provider: a plain answer, a
# task with tools, continuing the session, an edit plus a failing command,
# and a model that doesn't exist. It makes a handful of small requests, in a
# scratch folder with an empty home, so nothing of yours is sent.
#
#   bench/provider.sh ollama/qwen2.5:3b              # a local server
#   OLLAMA_HOST=127.0.0.1:11435 bench/provider.sh ollama/qwen2.5:3b
#   GROQ_API_KEY=... bench/provider.sh groq/llama-3.3-70b-versatile
#   BIN=target/release/claudejr bench/provider.sh openai/gpt-5
#
# Small local models often get the task itself wrong (the wrong text in the
# file); what is checked here is that the calls go out, come back and the
# conversation holds together, which the last column says.
set -u
model=${1:?usage: bench/provider.sh PROVIDER/MODEL}
cd "$(dirname "$0")/.."
bin=$(realpath "${BIN:-target/x86_64-unknown-linux-musl/release/claudejr}")
work=$(mktemp -d)
mkdir -p "$work/home" "$work/proj"
cd "$work/proj"
printf 'alpha\nbeta\ngamma\n' > notes.txt
keys=$(env | grep -E '^[A-Z_]+_API_KEY=|^OLLAMA_HOST=' | grep -v '^ANTHROPIC_' | sed 's/=.*//')
run() {
    local pass=()
    for k in $keys; do pass+=("$k=${!k}"); done
    env -i PATH="$PATH" HOME="$work/home" XDG_CONFIG_HOME="$work/home" XDG_DATA_HOME="$work/home" \
        CLAUDE_JR_OFFLINE_SECS=20 "${pass[@]}" timeout 600 "$bin" "$@" -m "$model" 2>"$work/err"
}
field() { python3 -c 'import sys,json; r=json.load(sys.stdin); print(r.get(sys.argv[1]))' "$1" 2>/dev/null; }
pass=0; fail=0
say() { if [ "$1" = ok ]; then pass=$((pass+1)); else fail=$((fail+1)); fi; printf '  %-4s %s\n' "$1" "$2"; }

out=$(run -p "Reply with exactly the word: pong" --output-format json)
[ "$(echo "$out" | field ok)" = True ] && echo "$out" | field result | grep -qi pong && say ok "a plain answer" || say FAIL "a plain answer: $(tail -1 "$work/err")"

out=$(run -y -p "Read notes.txt, create hello.txt containing the word done, then run 'wc -l notes.txt' and tell me the number." --log "$work/log" --output-format json)
steps=$(grep -c '"type":"step"' "$work/log" 2>/dev/null || echo 0)
[ "$(echo "$out" | field ok)" = True ] && [ "$steps" -ge 2 ] && [ -f hello.txt ] && say ok "tools: $steps calls made and answered" || say FAIL "tools ($steps calls): $(tail -1 "$work/err")"

out=$(run -y -c -p "Which file did you create a moment ago? Answer with just its name.")
echo "$out" | grep -q "hello" && say ok "the session continues with its history" || say FAIL "continuing: ${out:0:120} $(tail -1 "$work/err")"

out=$(run -y -p "Use the edit tool to replace beta with BETA in notes.txt. Then run 'ls /definitely-not-here' and tell me what happened." --output-format json)
[ "$(echo "$out" | field ok)" = True ] && grep -q BETA notes.txt && say ok "an edit, and a failed command reported back" || say FAIL "edit and failing command: $(tail -1 "$work/err")"

bad="${model%%/*}/no-such-model-here"
err=$(env -i PATH="$PATH" HOME="$work/home" XDG_CONFIG_HOME="$work/home" XDG_DATA_HOME="$work/home" CLAUDE_JR_OFFLINE_SECS=20 \
      $(for k in $keys; do printf '%s=%s ' "$k" "${!k}"; done) timeout 120 "$bin" -p hi -m "$bad" 2>&1 >/dev/null | tail -1)
case "$err" in error:*) say ok "a missing model is an error a person can read: ${err:0:90}" ;; *) say FAIL "a missing model: ${err:0:120}" ;; esac

rm -rf "$work"
echo "$model: $pass passed, $fail failed"
[ "$fail" = 0 ]
