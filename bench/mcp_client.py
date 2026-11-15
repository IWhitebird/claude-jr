# Checks `claudejr mcp` with the official MCP SDK's own client, which
# validates every message against the protocol's schema: the handshake, the
# tool list, and a whole job (start two tasks, wait, read, diff, merge, drop)
# against the fake API in a scratch repository.
#
#   python3 -m venv /tmp/v && /tmp/v/bin/pip install mcp
#   /tmp/v/bin/python bench/mcp_client.py target/release/claudejr
import asyncio, json, os, socket, subprocess, sys, tempfile

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

binary = os.path.realpath(sys.argv[1])
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def git(repo, *args):
    subprocess.run(["git", "-c", "user.name=T", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", *args],
                   cwd=repo, check=True, capture_output=True)


def field(obj, snake, camel):
    """The SDK renamed its fields between versions; take whichever it has."""
    return getattr(obj, snake) if hasattr(obj, snake) else getattr(obj, camel)


async def main():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    mock = subprocess.Popen([sys.executable, "-I", os.path.join(root, "bench/mock.py"), str(port)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    home = tempfile.mkdtemp()
    repo = os.path.join(home, "repo")
    os.mkdir(repo)
    git(repo, "init", "-q", "-b", "main")
    open(os.path.join(repo, "kept.txt"), "w").write("x\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "first")
    for key in ("user.name=T", "user.email=t@example.com", "commit.gpgsign=false"):
        git(repo, "config", *key.split("="))
    env = {"PATH": os.environ["PATH"], "HOME": home, "XDG_DATA_HOME": home, "XDG_CONFIG_HOME": home,
           "ANTHROPIC_API_KEY": "mock", "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}"}
    server = StdioServerParameters(command=binary, args=["mcp", "-y"], env=env, cwd=repo)
    try:
        async with stdio_client(server) as (read, write), ClientSession(read, write) as session:
            init = await session.initialize()
            info = field(init, "server_info", "serverInfo")
            print("handshake:", info.name, info.version, "protocol", field(init, "protocol_version", "protocolVersion"))
            assert init.instructions and "start_task" in init.instructions
            tools = (await session.list_tools()).tools
            print("tools:", ", ".join(t.name for t in tools))
            assert len(tools) == 9
            assert all(field(t, "input_schema", "inputSchema")["type"] == "object" and t.description for t in tools)

            async def call(name, **args):
                result = await session.call_tool(name, args)
                text = result.content[0].text
                assert not field(result, "is_error", "isError"), f"{name}: {text}"
                return text

            ids = [json.loads(await call("start_task", prompt=p))["id"] for p in ("add the demo file", "and once more")]
            waited = json.loads(await call("wait_tasks", ids=ids))
            assert [t["status"] for t in waited] == ["done", "done"], waited
            result = json.loads(await call("task_result", id=ids[0]))
            assert result["answer"].startswith("Done.") and result["files"][0]["path"] == "demo.txt", result
            assert "+TWO" in await call("task_diff", id=ids[0])
            assert "steps in all" in await call("task_log", id=ids[0])
            assert (await call("merge_task", id=ids[0])).startswith("merged ")
            assert (await call("drop_task", id=ids[1])).startswith("dropped ")
            assert open(os.path.join(repo, "demo.txt")).read() == "one\nTWO\nthree\n"
            assert len(json.loads(await call("list_tasks"))) == 1
            # A tool that fails reports it in its result, as the protocol has it.
            bad = await session.call_tool("task_result", {"id": "no-such-task"})
            assert field(bad, "is_error", "isError") and "no task" in bad.content[0].text
            await session.send_ping()
            print("a whole job through the SDK's client: ok")
    finally:
        mock.kill()


asyncio.run(main())
