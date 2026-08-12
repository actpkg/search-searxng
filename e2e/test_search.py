async def test_rejects_base_url_without_a_scheme(client, expect_error):
    # base_url without a scheme is rejected before any network access.
    await expect_error(
        client, "search", {"query": "rust wasm", "base_url": "not-a-url"},
        "std:invalid-args", contains="http://",
    )


async def test_rejects_a_blank_query(client, expect_error):
    # A blank query is rejected too — SearXNG would otherwise answer with an
    # empty page.
    await expect_error(
        client, "search", {"query": "   ", "base_url": "https://searx.example.org"},
        "std:invalid-args",
    )


async def test_reachable_but_not_json_names_the_fix(client, expect_error, dead_end_url):
    # Point the component at a server that is reachable but does not serve
    # JSON at /search (HTTP 404). The old hurl suite got this for free by
    # pointing `base_url` at the ACT-HTTP server hosting the component
    # itself; over stdio-only MCP there is no such server, so `dead_end_url`
    # stands in for it. Exercises the most common real-world failure — an
    # instance that is reachable but misconfigured — and asserts the error
    # names the fix rather than just the status code.
    await expect_error(
        client, "search", {"query": "rust wasm", "base_url": dead_end_url},
        "std:internal", contains="search.formats",
    )
