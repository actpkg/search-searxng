//! Drive the packed component through `act run --mcp` with a real MCP client.
//!
//! This replaces the python fastmcp/pytest suite that still sits next to this
//! file: the tests observe exactly what an agent observes, over the same
//! client stack (`rmcp`) the host bridge itself is built on.
//!
//! Env: WASM — path to the packed component (default: the component's
//!      release build output);
//!      ACT  — the act invocation (default `act`; `npx @actcore/act`, the
//!             component justfile's default, also works — whitespace-split,
//!             like the shlex.split the python conftest did).

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex as AsyncMutex;

/// `().serve(transport)` hands back the client-role service running over the
/// child process: role first, the unit client handler second.
type Client = rmcp::service::RunningService<rmcp::service::RoleClient, ()>;

fn wasm_path() -> PathBuf {
    PathBuf::from(std::env::var("WASM").unwrap_or_else(|_| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/wasm32-wasip2/release/component_search_searxng.wasm"
        )
        .into()
    }))
}

/// The ACT invocation, honouring the same override the component justfile
/// uses. Its default there is `npx @actcore/act` — two words — which cannot
/// be `argv[0]` for a non-shell spawn, so the value is whitespace-split into
/// program + leading args. Quoted paths with spaces are not a form this
/// fleet passes through `ACT`; a full shlex is deliberately not pulled in.
fn act_argv() -> Vec<String> {
    std::env::var("ACT")
        .unwrap_or_else(|_| "act".into())
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Spawn `act run <wasm> --mcp` with the grant this component needs.
///
/// Grants are NOT optional: the default policy mode is `ask` and a headless
/// run degrades it to deny. The declared ceiling is `host = "*"` (act.toml:
/// the instance address is caller-supplied, so no fixed host can be
/// declared), so opening the `wasi:http` class grants exactly what the
/// python conftest granted — narrowing to a real instance is the deployer's
/// job, as the act.toml description itself instructs.
fn act_command() -> tokio::process::Command {
    let argv = act_argv();
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.arg("run").arg(wasm_path()).arg("--mcp");
    cmd.args(["--allow", "wasi:http"]);
    cmd
}

fn spawn_transport() -> TokioChildProcess {
    TokioChildProcess::new(act_command()).expect("spawn act run --mcp")
}

async fn connect() -> Client {
    ().serve(spawn_transport())
        .await
        .expect("rmcp handshake with act run --mcp")
}

fn first_text_block(result: &rmcp::model::CallToolResult) -> &rmcp::model::TextContent {
    match result.content.first() {
        Some(rmcp::model::ContentBlock::Text(t)) => t,
        other => panic!("expected the first content block to be Text, got: {other:?}"),
    }
}

/// The kind and message of a failed call may arrive on either path: as a
/// JSON-RPC error response (`ErrorData.data` / `message`) or as an isError
/// result (`_meta` / text content). The python conftest's `expect_error`
/// fixture handled both; so does this. `call-tool` has no `result<>`
/// wrapper, so a guest reporting a failed call can only do it through
/// `tool-event::error` — which is the isError path here; the JSON-RPC path
/// stays handled for the non-guest failure modes.
async fn error_kind_of(client: &Client, params: CallToolRequestParams) -> Option<(String, String)> {
    match client.call_tool(params).await {
        Err(rmcp::ServiceError::McpError(e)) => {
            let kind = e
                .data
                .as_ref()
                .and_then(|d| d.get("dev.actcore/error-kind"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            kind.map(|k| (k, e.message.to_string()))
        }
        Ok(result) => {
            assert_eq!(result.is_error, Some(true), "call must fail: {result:?}");
            let kind = result
                .meta
                .as_ref()
                .and_then(|m| m.0.get("dev.actcore/error-kind"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let message = result
                .content
                .first()
                .and_then(|b| match b {
                    rmcp::model::ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            kind.map(|k| (k, message))
        }
        Err(other) => panic!("unexpected transport failure: {other:?}"),
    }
}

/// A local HTTP stub for the component to target — the rust analogue of the
/// python conftest's `HTTPServer` fixtures. Binds an ephemeral port, serves
/// from a detached task, and answers every request with `first_line`, the
/// given body (when non-empty: with Content-Type and Content-Length; when
/// empty: with none, so the connection close delimits it) and no more. Each
/// request line is recorded so a test can assert what the component actually
/// sent, not just that the call succeeded.
///
/// `search` only ever GETs — `build_url` puts every caller value into the
/// query string — so there is no request body to de-chunk: end-of-headers is
/// end-of-request.
async fn spawn_stub(
    first_line: &'static str,
    body: Vec<u8>,
    content_type: &'static str,
) -> (String, Arc<AsyncMutex<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub server");
    let addr = listener.local_addr().expect("stub server addr");
    let last_request = Arc::new(AsyncMutex::new(String::new()));
    let sink = last_request.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let sink = sink.clone();
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = Vec::with_capacity(1024);
                let mut chunk = [0u8; 1024];
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                let text = String::from_utf8_lossy(&buf);
                *sink.lock().await = text.lines().next().unwrap_or("").to_string();

                let mut response = format!("{first_line}\r\n");
                if !body.is_empty() {
                    response.push_str(&format!(
                        "Content-Type: {content_type}\r\nContent-Length: {}\r\n",
                        body.len()
                    ));
                }
                response.push_str("Connection: close\r\n\r\n");
                let _ = sock.write_all(response.as_bytes()).await;
                if !body.is_empty() {
                    let _ = sock.write_all(&body).await;
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), last_request)
}

/// The manifest probe from the python test_info.py: the packed artifact must
/// declare its name and a version. Also the fast-fail the python `wasm_path`
/// fixture provided — an unpacked wasm (raw `cargo build` output, no
/// `act:component` section) declares no ceiling, every grant is refused as
/// "outside ceiling", and the failures point anywhere but at the missing
/// metadata. The justfile's `test: build` ordering exists so this test finds
/// a packed artifact.
#[test]
fn manifest_reports_name_and_version() {
    let output = {
        let argv = act_argv();
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        cmd.args(["inspect", "component-manifest"])
            .arg(wasm_path())
            .output()
            .expect("run act inspect component-manifest")
    };
    assert!(
        output.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: Value = serde_json::from_slice(&output.stdout).expect("manifest is JSON");
    assert_eq!(
        manifest["std"]["name"], "search-searxng",
        "packed manifest must carry the component name"
    );
    assert!(
        manifest["std"]["version"].is_string(),
        "packed manifest must carry a version, got: {}",
        manifest["std"]["version"]
    );
}

#[tokio::test]
async fn component_exposes_its_tools() {
    let client = connect().await;
    // hurl tools.hurl: `$.tools` count >= 1.
    let tools = client.list_all_tools().await.expect("list_all_tools");
    assert!(
        !tools.is_empty(),
        "component must expose at least one tool"
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn search_is_listed_with_both_mandatory_args_required() {
    let client = connect().await;
    // hurl search.hurl's first block: the search tool is listed, and both
    // mandatory arguments are declared as required.
    let tools = client.list_all_tools().await.expect("list_all_tools");
    assert_eq!(
        tools[0].name, "search",
        "search must be the component's first tool, got: {:?}",
        tools.iter().map(|t| t.name.to_string()).collect::<Vec<_>>()
    );
    let required = tools[0]
        .input_schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        required.iter().any(|v| v == "query"),
        "query must be declared required, got: {required:?}"
    );
    assert!(
        required.iter().any(|v| v == "base_url"),
        "base_url must be declared required, got: {required:?}"
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn rejects_base_url_without_a_scheme() {
    let client = connect().await;
    // base_url without a scheme is rejected before any network access:
    // `http::Uri` parses "not-a-url" as a relative reference, so the guest
    // checks the scheme itself and names the fix in the message.
    let args = json!({"query": "rust wasm", "base_url": "not-a-url"})
        .as_object()
        .unwrap()
        .clone();
    let (kind, message) = error_kind_of(
        &client,
        CallToolRequestParams::new("search").with_arguments(args),
    )
    .await
    .expect("the failure must carry a named error kind");
    assert_eq!(
        kind, "std:invalid-args",
        "the scheme failure must surface as std:invalid-args"
    );
    assert!(
        message.contains("http://"),
        "expected the scheme check's message to name http://, got: {message}"
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn rejects_a_blank_query() {
    let client = connect().await;
    // A blank query is rejected too — SearXNG would otherwise answer with an
    // empty page. The python test asserted only the kind; kept faithful.
    let args = json!({"query": "   ", "base_url": "https://searx.example.org"})
        .as_object()
        .unwrap()
        .clone();
    let (kind, _message) = error_kind_of(
        &client,
        CallToolRequestParams::new("search").with_arguments(args),
    )
    .await
    .expect("the failure must carry a named error kind");
    assert_eq!(
        kind, "std:invalid-args",
        "the blank-query failure must surface as std:invalid-args"
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn reachable_but_not_json_names_the_fix() {
    let client = connect().await;
    // Point the component at a server that is reachable but does not serve
    // JSON at /search (HTTP 404, no body) — the python conftest's
    // `dead_end_url` fixture. Exercises the most common real-world failure —
    // an instance that is reachable but misconfigured — and asserts the
    // error names the fix rather than just the status code.
    let (dead_end_url, _requests) = spawn_stub("HTTP/1.1 404 Not Found", Vec::new(), "").await;
    let args = json!({"query": "rust wasm", "base_url": dead_end_url})
        .as_object()
        .unwrap()
        .clone();
    let (kind, message) = error_kind_of(
        &client,
        CallToolRequestParams::new("search").with_arguments(args),
    )
    .await
    .expect("the failure must carry a named error kind");
    assert_eq!(
        kind, "std:internal",
        "the 404 must surface as std:internal, not a caller error"
    );
    assert!(
        message.contains("search.formats"),
        "expected the error to name search.formats in settings.yml, got: {message}"
    );
    client.cancel().await.ok();
}

/// Beyond python parity: the python suite never exercised the success path —
/// every assertion it had was about failures. This one stands up a stub that
/// speaks SearXNG's wire format and checks what the tool promises on the way
/// out: the query URL `build_url` constructs, and the normalised response
/// shape kept deliberately identical across the search components.
#[tokio::test]
async fn search_returns_normalised_results_and_builds_the_query() {
    let client = connect().await;
    let wire = json!({
        "query": "rust wasm",
        "results": [
            {"title": "Rust and WebAssembly", "url": "https://rustwasm.github.io/",
             "content": " Rising popularity. ", "publishedDate": "2026-01-02T00:00:00Z",
             "score": 3.5, "engine": "duckduckgo", "category": "general"},
            {"title": "Bare result", "url": "https://example.org/two",
             "content": "", "publishedDate": ""}
        ],
        // answers has been both strings and {answer, url} objects across
        // SearXNG versions; unresponsive_engines is a [name, reason] pair.
        "answers": [{"answer": "42", "url": "https://example.org/answer"}],
        "suggestions": ["rust wasip2"],
        "unresponsive_engines": [["bing", "timeout"]]
    });
    let (base_url, request_line) = spawn_stub(
        "HTTP/1.1 200 OK",
        serde_json::to_vec(&wire).expect("stub body serializes"),
        "application/json",
    )
    .await;

    let args = json!({"query": "rust wasm", "base_url": base_url})
        .as_object()
        .unwrap()
        .clone();
    let result = client
        .call_tool(CallToolRequestParams::new("search").with_arguments(args))
        .await
        .expect("call_tool search");
    assert_ne!(result.is_error, Some(true), "search failed: {result:?}");

    // build_url: form-urlencoded q, the mandatory format=json, and none of
    // the optional filters (they were not supplied).
    let request = request_line.lock().await.clone();
    assert!(
        request.starts_with("GET /search?"),
        "expected a GET of /search, got: {request}"
    );
    assert!(
        request.contains("q=rust+wasm"),
        "expected the form-encoded query, got: {request}"
    );
    assert!(
        request.contains("format=json"),
        "expected format=json in the query, got: {request}"
    );

    // A plain Serialize struct is CBOR-encoded by the SDK, and the bridge
    // renders that both as a JSON text block and as structuredContent.
    let block = first_text_block(&result);
    let response: Value =
        serde_json::from_str(&block.text).expect("normalised response is JSON");
    assert_eq!(
        result.structured_content.as_ref(),
        Some(&response),
        "structuredContent must carry the same object as the text block"
    );

    assert_eq!(response["query"], "rust wasm");
    let results = response["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2, "both wire results must survive: {response}");
    assert_eq!(results[0]["title"], "Rust and WebAssembly");
    assert_eq!(results[0]["url"], "https://rustwasm.github.io/");
    assert_eq!(results[0]["snippet"], " Rising popularity. ");
    assert_eq!(results[0]["published"], "2026-01-02T00:00:00Z");
    assert_eq!(results[0]["score"], 3.5);
    assert_eq!(results[0]["engine"], "duckduckgo");
    assert_eq!(results[0]["category"], "general");
    // Empty strings are dropped into nulls, not passed through.
    assert!(
        results[1]["snippet"].is_null() && results[1]["published"].is_null(),
        "empty content/publishedDate must not surface, got: {}",
        results[1]
    );
    assert_eq!(response["answers"], json!(["42"]));
    assert_eq!(response["suggestions"], json!(["rust wasip2"]));
    assert_eq!(response["unresponsive_engines"], json!(["bing: timeout"]));

    client.cancel().await.ok();
}
