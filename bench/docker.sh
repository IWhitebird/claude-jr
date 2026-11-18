#!/usr/bin/env bash
# Builds the Docker image from a static Linux binary and checks it against the
# fake API (no key, no network): its size, a run with tools as an ordinary
# user, commands in busybox's sh, a command that overruns its time, and a
# session kept in a mounted home. CI runs it on every push.
#
#   bench/docker.sh [BINARY]     default: target/x86_64-unknown-linux-musl/release/claudejr
#   IMAGE=name bench/docker.sh   check an image that is already built
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
mkdir -p "$work/proj/sub" "$work/home"
image=${IMAGE:-claude-jr:check}
pass=0; fail=0
say() { if [ "$1" = ok ]; then pass=$((pass+1)); else fail=$((fail+1)); fi; printf '  %-4s %s\n' "$1" "$2"; }
field() { python3 -c 'import sys,json; print(json.load(sys.stdin).get(sys.argv[1]))' "$1" 2>/dev/null; }

# mock NAME=value...: a fake API with these settings, at $url.
mock() {
    [ -n "${mock_pid:-}" ] && kill "$mock_pid" 2>/dev/null
    rm -f "$work/port" "$work/requests"*
    env MOCK_LOG="$work/requests" "$@" python3 -I "$root/bench/mock.py" 0 >"$work/port" 2>/dev/null &
    mock_pid=$!
    for _ in $(seq 100); do [ -s "$work/port" ] && break; sleep 0.1; done
    url="http://127.0.0.1:$(head -1 "$work/port")"
}
trap '[ -n "${mock_pid:-}" ] && kill "$mock_pid" 2>/dev/null; rm -rf "$work"' EXIT

# jr ARGS...: the image as this user, in the project folder, talking to the fake API.
jr() {
    timeout 120 docker run --rm --network host --user "$(id -u):$(id -g)" \
        -e ANTHROPIC_API_KEY=mock -e ANTHROPIC_BASE_URL="$url" -e CLAUDE_JR_OFFLINE_SECS=15 \
        -v "$work/proj:/work" ${home:+-v "$home:/home/jr"} "$image" "$@" 2>"$work/err"
}

if [ -z "${IMAGE:-}" ]; then
    bin=$(realpath "${1:-$root/target/x86_64-unknown-linux-musl/release/claudejr}")
    arch=$(docker version --format '{{.Server.Arch}}')
    mkdir -p "$work/bin/$arch" && cp "$bin" "$work/bin/$arch/claudejr"
    docker build -q --build-context "bin=$work/bin" -t "$image" "$root" >/dev/null \
        && say ok "the image builds" || { say FAIL "docker build"; exit 1; }
    [ "$(docker run --rm "$image" --version)" = "$("$bin" --version)" ] \
        && say ok "it runs: $("$bin" --version)" || say FAIL "--version in the image"
fi

size=$(docker image inspect "$image" --format '{{.Size}}')
[ "$size" -lt 5000000 ] && say ok "the image is $((size / 1000)) KB" || say FAIL "the image is $((size / 1000)) KB, over 5 MB"

home=
mock
out=$(jr -y -p "do the demo" --output-format json)
[ "$(echo "$out" | field ok)" = True ] && [ "$(tr '\n' ' ' <"$work/proj/demo.txt")" = "one TWO three " ] \
    && say ok "a run with tools: write, read, edit, and a command in sh" || say FAIL "a run with tools: $out $(tail -2 "$work/err")"
grep -q 'xxxxxxxx' "$work/requests" && say ok "the command's output went back to the model" || say FAIL "no command output in the requests"
grep -q 'the bash tool runs sh' "$work/requests" && say ok "the model is told the shell is sh" || say FAIL "the prompt doesn't say the shell is sh"
[ "$(stat -c %u "$work/proj/demo.txt")" = "$(id -u)" ] && say ok "files it writes belong to the user who ran it" || say FAIL "demo.txt belongs to $(stat -c %U "$work/proj/demo.txt")"
[ ! -s "$work/requests.invalid" ] && say ok "nothing the API would refuse was sent" || say FAIL "refused: $(cat "$work/requests.invalid")"

mock MOCK_CMD='cd sub; pwd'
jr -y -p "go there" >/dev/null
grep -q 'working directory is now' "$work/requests" && say ok "cd carries over" || say FAIL "cd: $(tail -2 "$work/err")"

# A command that outlives its time is stopped along with what it started (busybox has its own kill).
mock MOCK_CMD='sleep 300 & sleep 300' MOCK_TIMEOUT=1 MOCK_ROUNDS=1
start=$(date +%s)
jr -y -p "wait too long" >/dev/null
took=$(( $(date +%s) - start ))
grep -q 'Timed out after 1s' "$work/requests" && [ "$took" -lt 30 ] \
    && say ok "a command over its time is stopped, with what it started (${took}s)" || say FAIL "timeout: ${took}s $(tail -2 "$work/err")"

# With a home that outlives the container, so does the conversation.
home=$work/home
mock
jr -y -p "remember the word plum" >/dev/null
out=$(jr -y -c -p "carry on" --output-format json)
[ "$(echo "$out" | field ok)" = True ] && grep -q 'remember the word plum' <(tail -1 "$work/requests") \
    && say ok "with a mounted home, the session continues" || say FAIL "continuing: $out $(tail -2 "$work/err")"

echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
