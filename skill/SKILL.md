---
name: search-searxng
description: Search the web through a self-hosted SearXNG instance — ranked results, no API key
metadata:
  act: {}
---

# SearXNG Search Component

Query a [SearXNG](https://docs.searxng.org/) instance and get back ranked web results.

SearXNG is a metasearch engine: it forwards your query to many upstream engines
(DuckDuckGo, Wikipedia, Startpage, …) and merges the results. Because you run the
instance yourself, **the query never leaves your perimeter** — there is no third-party
search vendor in the data path, and no API key to manage.

## Prerequisite: enable the JSON format

A SearXNG instance serves JSON **only** when `json` is listed under `search.formats`,
and it is *not* enabled by default. In your instance's `settings.yml`:

```yaml
search:
  formats:
    - html
    - json
```

Restart the instance afterwards. Without this the component fails with an error saying
exactly this — most public instances do not enable it, so this component expects your
own deployment.

## Tools

### search

```
search(query: "wasm component model", base_url: "http://searx.internal:8888")
```

| Argument | Required | Meaning |
|---|---|---|
| `query` | yes | What to search for. |
| `base_url` | yes | Instance root, e.g. `http://searx.internal:8888`. Must include the scheme. |
| `categories` | no | Comma-separated, e.g. `general,news`. Defaults to the instance's own. |
| `engines` | no | Restrict to specific engines, e.g. `duckduckgo,wikipedia`. |
| `language` | no | `en`, `en-US`, `all`, … |
| `time_range` | no | `day`, `week`, `month`, `year`. |
| `safesearch` | no | `0` off, `1` moderate, `2` strict. |
| `pageno` | no | Result page, starting at 1. |
| `timeout_ms` | no | Request timeout, default 15000. |

Returns:

```json
{
  "query": "wasm component model",
  "results": [
    {
      "title": "…", "url": "https://…", "snippet": "…",
      "published": null, "score": 2.5,
      "engine": "duckduckgo", "category": "general"
    }
  ],
  "answers": ["…"],
  "suggestions": ["…"],
  "unresponsive_engines": ["google: timeout"]
}
```

**Check `unresponsive_engines` when results look thin.** A short result list is usually
explained by upstream engines failing rather than by the query itself.

## Fetching the pages

This component returns links and snippets, not page content. To read the results, pass
the URLs to `http-client` and convert with an HTML→Markdown component. Keeping those
steps separate means each one declares its own, narrower network ceiling.

## Capability

Declares `wasi:http` with `host = "*"`, because the instance address is supplied by the
caller and cannot be known at build time. **Narrow it at deployment** to your own
instance:

```bash
act call search-searxng.wasm search \
  --args '{"query":"…","base_url":"http://searx.internal:8888"}' \
  --grant '{"wasi:http":{"mode":"allowlist","allow":[{"host":"searx.internal"}]}}'
```

With that grant the component can reach your instance and nothing else — and the host
logs every request it makes.
