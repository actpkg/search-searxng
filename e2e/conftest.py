"""Shared fixtures for the MCP-driven e2e suite.

The suite drives the packed component through `act run --mcp` over stdio with
a real MCP client, so what the tests observe is what an agent observes.
"""

import asyncio
import json
import os
import shlex
import subprocess
import threading
import pytest
from http.server import BaseHTTPRequestHandler, HTTPServer
from contextlib import AsyncExitStack
from pathlib import Path

from fastmcp import Client
from fastmcp.client.transports import StdioTransport

# Measured in docs/specs/2026-08-08-e2e-harness-findings.md, question 1.
from mcp.shared.exceptions import McpError

WASM = "target/wasm32-wasip2/release/component_search_searxng.wasm"

# ACT's audit trail writes to stderr unconditionally — it is not governed by
# RUST_LOG — so it is redirected to a file rather than left to flood pytest.
LOG_FILE = Path(".pytest-act-stderr.log")

# Healthy connects are measured in fractions of a second; this only has
# to be loose enough never to trip on a slow runner.
CONNECT_TIMEOUT = 30


@pytest.fixture(scope="session")
def act_command() -> list[str]:
    """The ACT invocation, honouring the same override the justfile uses.

    Parsed with shlex, not treated as a single path: the justfile's own
    default for its `act` variable is `npx @actcore/act` — two words — which
    cannot be `argv[0]` for a non-shell `subprocess.run`/`StdioTransport`
    call. A bare `os.environ.get("ACT", "act")` string breaks that default;
    splitting it is what makes both forms ("act" on PATH, and the npx
    two-word default) actually spawn.
    """
    return shlex.split(os.environ.get("ACT", "act"))


@pytest.fixture(scope="session")
def wasm_path(act_command: list[str]) -> Path:
    """The packed component.

    Existence is not enough and neither is a fresh mtime: `cargo build`
    produces a wasm with no `act:component` custom section, and an unpacked
    artifact declares no capability ceiling, so every grant is refused as
    "outside ceiling" and the failures point anywhere but here. This has
    already bitten this workspace repeatedly, so the fixture checks the
    section rather than the file.
    """
    path = Path(WASM)
    if not path.exists():
        pytest.fail(f"{path} is missing — run `just build && just pack` first")
    probe = subprocess.run(
        [*act_command, "inspect", "component-manifest", str(path)],
        capture_output=True, text=True,
    )
    name = json.loads(probe.stdout or "{}").get("std", {}).get("name", "unknown")
    if name in ("", "unknown"):
        pytest.fail(f"{path} is built but not packed — run `just pack`")
    return path


@pytest.fixture
async def client(act_command: list[str], wasm_path: Path):
    """A connected MCP client, one `act` process per test.

    `--allow wasi:http` moves here verbatim from the old justfile's `act run
    ... --allow wasi:http`: every real call the guest makes is an outbound
    HTTP request to a caller-supplied `base_url`, so every test needs it —
    there is no hurl case in the old suite that exercises a denied-http
    path to protect by withholding the grant.
    """
    transport = StdioTransport(
        command=act_command[0],
        args=[*act_command[1:], "run", str(wasm_path), "--mcp", "--allow", "wasi:http"],
        keep_alive=False,  # stateless per call — fresh process is the safe default
        log_file=LOG_FILE,
    )
    async with AsyncExitStack() as stack:
        # Bound the connect, not the test body. A stalled handshake otherwise
        # consumes the whole pytest timeout with no diagnostic at all — which
        # is precisely how the webdriver-bidi CI hang presented for hours.
        try:
            async with asyncio.timeout(CONNECT_TIMEOUT):
                connected = await stack.enter_async_context(Client(transport))
        except TimeoutError:
            pytest.fail(
                f"MCP client did not connect within {CONNECT_TIMEOUT}s; "
                f"act's stderr, if it wrote any, is dumped at session end"
            )
        yield connected


class _NotFoundHandler(BaseHTTPRequestHandler):
    """Answers every request with a bare 404 and no body."""

    def do_GET(self):  # noqa: N802 - http.server's naming convention
        self.send_response(404)
        self.end_headers()

    def log_message(self, *args):
        pass  # keep pytest's output free of one line per request


@pytest.fixture
def dead_end_url():
    """A local HTTP server that is reachable but serves nothing useful.

    Stands in for the old hurl suite's `base_url: "{{baseurl}}"` trick, which
    pointed `search` at the ACT-HTTP server hosting the component itself —
    convenient there because that server was already running, guaranteed to
    answer 404 for `/search`, and gone the moment the transport moved to
    stdio-only MCP with no HTTP listener of its own. This fixture reproduces
    the same "reachable, not serving JSON, HTTP 404" shape without spinning
    up a second `act run --http` process just to have something to point at.

    `HTTPServer.__init__` binds and starts listening synchronously — the
    socket is already accepting connections before `serve_forever` ever
    runs, so (unlike a spawned subprocess) there is no window where the
    port isn't ready yet and nothing to poll for.
    """
    server = HTTPServer(("127.0.0.1", 0), _NotFoundHandler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{port}"
    finally:
        server.shutdown()
        thread.join(timeout=5)


@pytest.fixture
def expect_error():
    """Assert a call fails with a specific ACT error kind (and, optionally, a
    substring of its human-readable message).

    Exposed as a fixture rather than a plain function so tests never have to
    import from `conftest` — that import only resolves when the test
    directory happens to be on `sys.path`, which is not something to rely on.

    Measured, not assumed. `call-tool` in `act:tools` returns a bare
    `tool-result` with NO `result<>` wrapper — only `list-tools` has one — so
    a guest reporting a failed tool call can only do it through
    `tool-event::error`, which arrives as a result with `is_error` set and the
    kind in `_meta`, and the message as its one text content part. **That is
    the path a tool test will take.**

    The JSON-RPC error path exists for failures that are not the guest's tool
    body: `list-tools`, the session operations, a wasmtime trap, an
    unreachable actor. It raises `mcp.shared.exceptions.McpError`, with the
    kind at `exc.error.data` and the message at `exc.error.message`. Both are
    handled here so callers need not care.
    """

    async def _expect(client, tool: str, arguments: dict, kind: str, contains: str | None = None):
        try:
            result = await client.call_tool(tool, arguments, raise_on_error=False)
        except McpError as exc:
            data = getattr(getattr(exc, "error", None), "data", None) or {}
            assert data.get("dev.actcore/error-kind") == kind, (
                f"expected {kind} on the JSON-RPC error path, got {data!r}"
            )
            if contains is not None:
                message = getattr(exc.error, "message", "") or ""
                assert contains in message, f"expected {contains!r} in {message!r}"
            return

        assert result.is_error, f"expected {tool} to fail, got {result!r}"
        meta = result.meta or {}
        assert meta.get("dev.actcore/error-kind") == kind, (
            f"expected {kind} on the isError path, got {meta!r}"
        )
        if contains is not None:
            message = result.content[0].text if result.content else ""
            assert contains in message, f"expected {contains!r} in {message!r}"

    return _expect


def pytest_sessionfinish(session, exitstatus):
    """Print act's stderr when the run did not pass.

    `log_file` keeps the audit trail out of the test output, which is right
    for a green run and wrong for every other kind: on an ephemeral CI runner
    nothing ever reads that file. Diagnosing a CI-only hang in this fleet
    cost several rounds of probing that one line of this stream would have
    answered. A hook rather than a fixture finaliser on purpose — fixture
    teardown does not run when the session dies mid-test.
    """
    if exitstatus == 0 or not LOG_FILE.exists():
        return
    text = LOG_FILE.read_text(errors="replace").strip()
    if text:
        print(f"\n--- act stderr ({LOG_FILE}) ---\n{text}")
