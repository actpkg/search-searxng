//! Search the web through a self-hosted SearXNG instance.
//!
//! SearXNG is a metasearch engine: it queries many upstream engines and merges the
//! results. This component deliberately does *not* reimplement that — maintaining
//! result parsers for a hundred-plus engines is a continuous treadmill. SearXNG does
//! that work; we consume its stable JSON API.
//!
//! The instance address is supplied by the caller, so the declared `wasi:http` ceiling
//! is `host = "*"` and must be narrowed at deployment. That is an honest limitation of
//! any component pointed at a self-hosted service.

use act_sdk::prelude::*;
use serde::Serialize;

const DEFAULT_TIMEOUT_MS: u64 = 15_000;

// ── Public (normalised) shape ────────────────────────────────────────────────
// Kept deliberately identical across search components so they are interchangeable
// in a dataflow graph. Duplicated rather than shared via a crate until the schema
// settles and there are three or more consumers.

/// One search result.
#[derive(Serialize, JsonSchema)]
struct SearchResult {
    /// Result title.
    title: String,
    /// Result URL.
    url: String,
    /// Short extract, when the upstream engine provided one.
    snippet: Option<String>,
    /// Publication date as reported upstream; format varies by engine.
    published: Option<String>,
    /// SearXNG's merged relevance score.
    score: Option<f64>,
    /// Upstream engine that produced this result.
    engine: Option<String>,
    /// SearXNG category, e.g. `general` or `news`.
    category: Option<String>,
}

/// A normalised search response.
#[derive(Serialize, JsonSchema)]
struct SearchResponse {
    /// The query as echoed back by the instance.
    query: String,
    /// Merged, ranked results.
    results: Vec<SearchResult>,
    /// Instant answers, when any engine supplied one.
    answers: Vec<String>,
    /// Spelling or query suggestions.
    suggestions: Vec<String>,
    /// Engines that failed to respond, with the reason when given. Worth surfacing:
    /// a thin result set is usually explained here rather than by the query.
    unresponsive_engines: Vec<String>,
}

// ── SearXNG wire format ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    query: String,
    #[serde(default)]
    results: Vec<WireResult>,
    #[serde(default)]
    answers: Vec<serde_json::Value>,
    #[serde(default)]
    suggestions: Vec<String>,
    #[serde(default)]
    unresponsive_engines: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct WireResult {
    #[serde(default)]
    title: String,
    url: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default, rename = "publishedDate")]
    published_date: Option<String>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    category: Option<String>,
}

/// Flatten one of SearXNG's loosely-typed list entries to a string.
///
/// The shapes vary across versions: `answers` has been both a list of strings and a
/// list of `{answer, url}` objects, and `unresponsive_engines` is a `[name, reason]`
/// pair. Accept all of them rather than pinning one instance version.
fn flatten(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(map) => map
            .get("answer")
            .or_else(|| map.get("name"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        serde_json::Value::Array(items) => {
            let parts: Vec<&str> = items.iter().filter_map(serde_json::Value::as_str).collect();
            (!parts.is_empty()).then(|| parts.join(": "))
        }
        _ => None,
    }
}

/// Build the `/search` URL, percent-encoding every caller-supplied value.
fn build_url(args: &SearchArgs) -> ActResult<String> {
    let base = args.base_url.trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(ActError::invalid_args(format!(
            "base_url must start with http:// or https://, got: {}",
            args.base_url
        )));
    }
    if args.query.trim().is_empty() {
        return Err(ActError::invalid_args("query must not be empty"));
    }

    let mut query = form_urlencoded::Serializer::new(String::new());
    query.append_pair("q", &args.query);
    query.append_pair("format", "json");
    if let Some(v) = &args.categories {
        query.append_pair("categories", v);
    }
    if let Some(v) = &args.engines {
        query.append_pair("engines", v);
    }
    if let Some(v) = &args.language {
        query.append_pair("language", v);
    }
    if let Some(v) = &args.time_range {
        query.append_pair("time_range", v);
    }
    if let Some(v) = args.safesearch {
        query.append_pair("safesearch", &v.to_string());
    }
    if let Some(v) = args.pageno {
        query.append_pair("pageno", &v.to_string());
    }

    Ok(format!("{base}/search?{}", query.finish()))
}

/// Turn a non-success status into an actionable error.
///
/// A SearXNG instance serves JSON only when `json` is listed under `search.formats`
/// in `settings.yml`, and it is **not** enabled by default. That misconfiguration is
/// by far the most common failure here, so name it explicitly instead of reporting a
/// bare status code.
fn status_error(status: u16) -> ActError {
    if matches!(status, 403..=405) {
        ActError::internal(format!(
            "SearXNG returned HTTP {status}. This usually means the JSON output format \
             is disabled — add `json` to `search.formats` in the instance's settings.yml \
             and restart it. (Most public instances do not enable it; this component \
             expects your own deployment.)"
        ))
    } else {
        ActError::internal(format!("SearXNG returned HTTP {status}"))
    }
}

#[derive(Deserialize, JsonSchema)]
struct SearchArgs {
    /// Search query.
    query: String,
    /// Base URL of the SearXNG instance, e.g. `https://searx.example.org`.
    base_url: String,
    /// Comma-separated categories, e.g. `general,news`. Defaults to the instance's own.
    categories: Option<String>,
    /// Comma-separated engines to restrict the search to, e.g. `duckduckgo,wikipedia`.
    engines: Option<String>,
    /// Language code, e.g. `en`, `en-US`, or `all`.
    language: Option<String>,
    /// Restrict results by age: `day`, `week`, `month` or `year`.
    time_range: Option<String>,
    /// Safe search level: 0 (off), 1 (moderate) or 2 (strict).
    safesearch: Option<u8>,
    /// Result page number, starting at 1.
    pageno: Option<u32>,
    /// Request timeout in milliseconds (default 15000).
    timeout_ms: Option<u64>,
}

#[act_component]
mod component {
    use super::*;

    #[act_tool(
        description = "Search the web via a SearXNG instance and return ranked results",
        read_only
    )]
    async fn search(#[args] args: SearchArgs) -> ActResult<SearchResponse> {
        let url = build_url(&args)?;

        let response = wasi_fetch::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_millis(
                args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
            ))
            .send()
            .await
            .map_err(|e| match e {
                wasi_fetch::Error::Url(msg) => ActError::invalid_args(msg),
                other => ActError::internal(format!("Cannot reach SearXNG: {other}")),
            })?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(status_error(status));
        }

        let body = response
            .into_body()
            .text()
            .await
            .map_err(|e| ActError::internal(format!("Cannot read SearXNG response: {e}")))?;

        let wire: WireResponse = serde_json::from_str(&body).map_err(|e| {
            ActError::internal(format!(
                "SearXNG response was not valid JSON ({e}). The instance most likely has \
                 the JSON format disabled — add `json` to `search.formats` in settings.yml."
            ))
        })?;

        Ok(SearchResponse {
            query: if wire.query.is_empty() {
                args.query.clone()
            } else {
                wire.query
            },
            results: wire
                .results
                .into_iter()
                .map(|r| SearchResult {
                    title: r.title,
                    url: r.url,
                    snippet: r.content.filter(|s| !s.is_empty()),
                    published: r.published_date.filter(|s| !s.is_empty()),
                    score: r.score,
                    engine: r.engine,
                    category: r.category,
                })
                .collect(),
            answers: wire.answers.iter().filter_map(flatten).collect(),
            suggestions: wire.suggestions,
            unresponsive_engines: wire
                .unresponsive_engines
                .iter()
                .filter_map(flatten)
                .collect(),
        })
    }
}
