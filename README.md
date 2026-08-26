# Glimse

Glimse is a local service for visual output produced by terminal-based AI agents. The daemon listens on loopback and persists immutable publications under a per-user store root. The structured CLI publishes local files, reads daemon state, closes sessions, and returns machine-readable JSON.

The checked HTTP contract is [`docs/openapi-v1.json`](docs/openapi-v1.json). The canonical CLI schemas are [`docs/cli-publish-v1.schema.json`](docs/cli-publish-v1.schema.json) and [`docs/cli-output-v1.schema.json`](docs/cli-output-v1.schema.json). The daemon configuration schema is [`docs/config-v1.schema.json`](docs/config-v1.schema.json). The browser feed receives live updates. Service management remains pending.

The agreed product design is recorded in [`docs/product-design.md`](docs/product-design.md). The dependency-ordered build plan is in [`docs/implementation-plan.md`](docs/implementation-plan.md).

## Supported development tools

- Rust 1.93.1, selected by `rust-toolchain.toml`
- Node.js 22.12 through 24.x; CI and `.nvmrc` use Node.js 22
- npm with the selected Node.js installation

Install the frontend dependencies from the lockfile after cloning.

```bash
cd web
npm ci
cd ..
```

## Development commands

The Make targets keep local checks aligned with CI.

```bash
make build           # Build the frontend, then the Rust binary
make check-rust      # Build frontend assets; run rustfmt, Clippy, and Rust tests
make check-frontend  # Run TypeScript checks, frontend tests, and the production build
make check           # Run every frontend and Rust check
```

Clippy runs with warnings denied. Cargo commands use `Cargo.lock`, and frontend installation uses `web/package-lock.json`. Run the daemon after building. No arguments and the explicit `daemon` command are equivalent.

```bash
make build
cargo run --locked
cargo run --locked -- daemon
```

The daemon listens on `127.0.0.1:3030` by default. It reads a bounded, versioned JSON configuration from `$XDG_CONFIG_HOME/glim/config.json` when `XDG_CONFIG_HOME` is set, or from `$HOME/.config/glim/config.json` otherwise. Set `GLIM_CONFIG` to an absolute path when a different file is required; a missing explicit file is an error. Environment values override file values.

```json
{
  "schema_version": 1,
  "store_root": "/home/user/.local/share/glim",
  "bind": "127.0.0.1:3030"
}
```

`GLIM_STORE_ROOT` overrides `store_root`. Without either value, the daemon uses `$XDG_DATA_HOME/glim`, then `$HOME/.local/share/glim`. `GLIM_BIND` overrides `bind`. Local mode accepts numeric loopback addresses with nonzero ports.

```bash
GLIM_STORE_ROOT=/tmp/glim-store GLIM_BIND=127.0.0.1:4040 cargo run --locked
curl http://127.0.0.1:4040/api/v1/health
```

Token mode protects feed pages, API routes, SSE, ranges, media, and downloads. The health endpoint, login shell, and compiled frontend assets remain public. A non-loopback bind requires token mode, a configured HTTPS origin, and PEM certificate and private-key files.

```json
{
  "schema_version": 1,
  "store_root": "/home/user/.local/share/glim",
  "bind": "0.0.0.0:3443",
  "access": {
    "mode": "token",
    "token_file": "/home/user/.config/glim/access-token",
    "public_origin": "https://glim.example:3443",
    "tls_certificate": "/home/user/.config/glim/cert.pem",
    "tls_private_key": "/home/user/.config/glim/key.pem"
  }
}
```

The daemon creates a missing token as 32 random bytes encoded by 64 lowercase hexadecimal characters. On Linux the token file has mode `0600`; startup rejects symlinks, malformed values, and group- or world-accessible files. Certificate provisioning remains the operator's responsibility. `GLIM_ACCESS_MODE`, `GLIM_TOKEN_FILE`, `GLIM_PUBLIC_ORIGIN`, `GLIM_TLS_CERTIFICATE`, and `GLIM_TLS_PRIVATE_KEY` override their file values.

API clients use the token as a Bearer credential. The browser login form exchanges it for a bounded 12-hour HttpOnly `SameSite=Strict` session and never puts the token in a URL. Cookie-authenticated mutations require the configured exact origin. Sandboxed HTML receives a renewable five-minute capability restricted to one file's declared support subtree; iframe content never receives the persistent token or browser cookie.

Trusted-proxy mode delegates TLS and user authentication to a reverse proxy. Glimse authorizes only the immediate TCP peer IP and ignores `Forwarded`, `X-Forwarded-For`, `X-Real-IP`, `X-Forwarded-Host`, and `X-Forwarded-Proto`. The allowlist contains exact numeric IP addresses, not hostnames or CIDRs. Health remains public for probes; every other route requires an allowlisted peer. Token login and session endpoints are unavailable in this mode. Browser-originated mutations require an exact `Origin` match; headerless non-browser API clients rely on the trusted peer boundary.

```json
{
  "schema_version": 1,
  "store_root": "/home/user/.local/share/glim",
  "bind": "0.0.0.0:3030",
  "access": {
    "mode": "trusted_proxy",
    "trusted_proxy_ips": ["100.64.0.10"],
    "public_origin": "https://glim.example"
  }
}
```

`GLIM_ACCESS_MODE=trusted_proxy`, `GLIM_TRUSTED_PROXY_IPS=100.64.0.10,100.64.0.11`, and `GLIM_PUBLIC_ORIGIN=https://glim.example` provide the corresponding environment configuration. A non-loopback bind requires an HTTPS public origin, but Glimse serves plaintext HTTP to the proxy because the proxy terminates TLS.

Configure a reverse proxy or Tailscale Serve deployment so only the proxy can reach the Glimse listener, then allowlist the source IP that Glimse observes for that connection. The proxy must enforce the intended user or private-network boundary and preserve browser `Origin` and `Sec-Fetch-Site` headers while forwarding all HTTP methods, SSE streams, range headers, and response streaming unchanged. Do not use forwarded identity headers as authorization. The automated tests exercise the Glimse side with real TCP sockets; they do not certify a specific nginx, Caddy, or Tailscale Serve configuration.

The frontend build writes `web/dist/index.html`, `web/dist/assets/app.js`, and the bundled PDF.js worker at `web/dist/assets/pdf.worker.mjs`. Rust embeds all three files, so the resulting binary does not read frontend files at runtime.

The browser serves the global feed at `/feed` (and `/`), session feeds at `/sessions/{public_id}`, and project feeds at `/projects/{project_id}`. Project page IDs are limited to positive integers no greater than JavaScript's safe-integer maximum (9,007,199,254,740,991). The static viewer supports sanitized commentary and Markdown artifacts, images and SVG, text, JSON, bounded CSV tables, native video and audio controls, lazy PDF.js pages, sandboxed HTML, and downloads. Media sources are released outside a 1,000-pixel vertical margin. PDF pages materialize within a 1,500-pixel margin, use 64 KiB range chunks, and retain at most three canvases per artifact.

HTML renders inline with scripts disabled. The renderer removes nested frames, plugins, active forms, document policies, and navigation links, then rewrites declared resources to the artifact's exact support path or scoped capability path. Each HTML artifact provides a warning control for reloading with `allow-scripts`; no other sandbox permission is lifted. The content security policy continues to block ordinary network APIs and undeclared subresources in script mode. Browser sandboxing cannot prevent a script from navigating its own frame, which can make a network request, and the warning states this limitation before scripts run. Script mode is never selected automatically.

Each valid feed route opens a scoped server-sent event stream. The daemon retains 256 live events and replays at most 100 durable posts after `Last-Event-ID`; lag or a larger replay emits `reset` so the browser reloads the latest page. Post events contain the complete API `Post` object and use the positive post ID as the SSE ID. Session closure emits `session-closed`. The browser deduplicates post IDs and orders posts by `published_at DESC, id DESC`.

At the top of the page, live posts enter the feed immediately. Away from the top, the browser retains at most 100 pending posts without changing the feed or viewport. The new-content control merges that queue and returns focus to the newest post. A larger burst switches to reconciliation instead of retaining more data. Session pages send a heartbeat every 30 seconds only while visible and while SSE is open. They also provide a confirmed close control that stops live work and releases renderer resources after successful deletion.

## CLI

Every short-lived CLI command writes one JSON value to standard output. Failures use the same output channel and exit nonzero. A committed publication remains successful if an explicitly requested browser launch fails; `result.browser_launch` reports the launch outcome. `GLIM_DAEMON_URL` accepts an HTTP or HTTPS origin and defaults to `http://127.0.0.1:3030`. In token mode the CLI reads the configured token file and adds the Bearer credential without including it in output URLs.

Canonical publication reads versioned JSON from standard input.

```bash
cat <<'JSON' | glim publish --json
{"schema_version":1,"integration_namespace":"pi","external_session_key":"session-1","project_label":"analysis","working_directory":"/work/analysis","title":"Population response","commentary":"The response increased.\n\nError bars show SEM.","files":[{"source_path":"/work/analysis/plot.png","caption":"Mean response"}]}
JSON
```

A one-file publication can use flags. `--commentary-file` avoids shell quoting for multiline Markdown. Add `--open` only when the command should launch a browser.

```bash
glim publish --file plot.png --integration pi --external-key session-1 \
  --project analysis --working-directory "$PWD" --title "Population response" \
  --commentary-file commentary.md --caption "Mean response"
glim status
glim list --session PUBLIC_ID --limit 20
glim list --project PROJECT_ID
glim list --global
glim show POST_ID
glim open PUBLIC_ID
glim close PUBLIC_ID
```

Markdown image references and HTML resource attributes collect local allowlisted files beneath the entry document directory. Linked CSS is tokenized recursively to collect `@import` and `url()` dependencies with bounded depth and reference counts. Collection ignores remote, fragment, data, and blob references. Invalid encodings, absolute paths, escaping traversal, unsupported files, special files, and escaping symlinks reject publication before any HTTP request.

Markdown, HTML, and CSS parsers require complete UTF-8 text. The CLI therefore holds a transient parser input allocation while collecting references, regardless of entry size. Artifact upload remains streamed, parser text is released before the HTTP request, and the daemon retains its streaming memory boundary. Inline `<style>` blocks and HTML `style` attributes remain deferred to the renderer slice.

The CLI records the Git root, branch, and commit when available. It does not collect remotes, diffs, environment variables, usernames, or repository contents.

## Compatibility

[`docs/compatibility.md`](docs/compatibility.md) defines the SQLite migration policy, HTTP API versioning policy, and CLI JSON schema policy.

## License

Glimse is available under the [MIT License](LICENSE).
