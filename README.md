# search-searxng

Search the web through a self-hosted [SearXNG](https://docs.searxng.org/) instance —
ranked results, no API key, and the query never leaves your perimeter.

SearXNG is a metasearch engine that queries many upstream engines and merges the
results. This component does not reimplement that: maintaining result parsers for a
hundred-plus engines is a continuous treadmill, and SearXNG already does it. We consume
its stable JSON API.

```bash
act call search-searxng.wasm search \
  --args '{"query":"wasm component model","base_url":"http://searx.internal:8888"}' \
  --grant '{"wasi:http":{"mode":"allowlist","allow":[{"host":"searx.internal"}]}}'
```

## Requires JSON output on the instance

SearXNG serves JSON only when `json` appears under `search.formats` in `settings.yml`,
and it is **not** enabled by default:

```yaml
search:
  formats:
    - html
    - json
```

Most public instances leave it off, so this component expects your own deployment. If
it is missing, the component says so explicitly rather than reporting a bare status code.

## Capability

Declares `wasi:http` with `host = "*"`. The instance address is supplied by the caller,
so no fixed host can be baked into the manifest — the same honest limitation any
component pointed at a self-hosted service has. Narrow it with a `--grant` at
deployment, as above; the host then logs every request the component makes.

## Usage

```bash
just init   # first time: fetch WIT deps
just build  # build wasm component
just test   # run e2e tests
```

## Publishing

Pushing to `main` publishes a signed component to
`actpkg.dev/<owner>/search-searxng` (owner derived from the git remote;
override the full path with the `OCI_REGISTRY` env var). CI signs the image
keylessly with [cosign](https://docs.sigstore.dev/) via GitHub OIDC.

One-time setup: create a Personal Access Token at
[actpkg.dev](https://actpkg.dev) and add it as a repository secret named
**`ACTPKG_TOKEN`** (Settings → Secrets and variables → Actions).

```bash
just publish   # local publish (unsigned); CI signs on push to main
```

## License

MIT OR Apache-2.0
