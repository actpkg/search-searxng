# The search tool is listed, and both mandatory arguments are declared as required.
POST {{baseurl}}/tools
HTTP 200
[Asserts]
jsonpath "$.tools[0].name" == "search"
jsonpath "$.tools[0].parameters_schema.required" contains "query"
jsonpath "$.tools[0].parameters_schema.required" contains "base_url"

# base_url without a scheme is rejected before any network access.
POST {{baseurl}}/tools/search
{"arguments": {"query": "rust wasm", "base_url": "not-a-url"}}
HTTP 422
[Asserts]
jsonpath "$.error.kind" == "std:invalid-args"
jsonpath "$.error.message" contains "http://"

# A blank query is rejected too — SearXNG would otherwise answer with an empty page.
POST {{baseurl}}/tools/search
{"arguments": {"query": "   ", "base_url": "https://searx.example.org"}}
HTTP 422
[Asserts]
jsonpath "$.error.kind" == "std:invalid-args"

# Point the component at the host serving it: /search does not exist there, so this
# exercises the most common real-world failure — an instance that is reachable but
# not serving JSON — and asserts the error names the fix rather than the status code.
POST {{baseurl}}/tools/search
{"arguments": {"query": "rust wasm", "base_url": "{{baseurl}}"}}
HTTP 500
[Asserts]
jsonpath "$.error.kind" == "std:internal"
jsonpath "$.error.message" contains "search.formats"
