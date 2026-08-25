# Glimse

Glimse is a local service for visual output produced by terminal-based AI agents. The daemon listens on loopback and persists immutable publications under a per-user store root. The structured CLI publishes local files, reads daemon state, closes sessions, and returns machine-readable JSON.

The checked HTTP contract is [`docs/openapi-v1.json`](docs/openapi-v1.json). The canonical CLI schemas are [`docs/cli-publish-v1.schema.json`](docs/cli-publish-v1.schema.json) and [`docs/cli-output-v1.schema.json`](docs/cli-output-v1.schema.json). The daemon configuration schema is [`docs/config-v1.schema.json`](docs/config-v1.schema.json). The browser feed receives live updates. Authentication and service management remain pending.

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

`GLIM_STORE_ROOT` overrides `store_root`. Without either value, the daemon uses `$XDG_DATA_HOME/glim`, then `$HOME/.local/share/glim`. `GLIM_BIND` overrides `bind`. Bind values must contain a numeric loopback address and a nonzero port. Non-loopback startup fails until a later Phase 4 slice adds authenticated access.

```bash
GLIM_STORE_ROOT=/tmp/glim-store GLIM_BIND=127.0.0.1:4040 cargo run --locked
curl http://127.0.0.1:4040/api/v1/health
```

The frontend build writes `web/dist/index.html`, `web/dist/assets/app.js`, and the bundled PDF.js worker at `web/dist/assets/pdf.worker.mjs`. Rust embeds all three files, so the resulting binary does not read frontend files at runtime.

The browser serves the global feed at `/feed` (and `/`), session feeds at `/sessions/{public_id}`, and project feeds at `/projects/{project_id}`. Project page IDs are limited to positive integers no greater than JavaScript's safe-integer maximum (9,007,199,254,740,991). The static viewer supports sanitized commentary and Markdown artifacts, images and SVG, text, JSON, bounded CSV tables, native video and audio controls, lazy PDF.js pages, sandboxed HTML, and downloads. Media sources are released outside a 1,000-pixel vertical margin. PDF pages materialize within a 1,500-pixel margin, use 64 KiB range chunks, and retain at most three canvases per artifact.

HTML renders inline with scripts disabled. The renderer removes nested frames, plugins, active forms, document policies, and navigation links, then rewrites declared resources to the artifact's exact support path. Each HTML artifact provides a warning control for reloading with `allow-scripts`; no other sandbox permission is lifted. The content security policy continues to block ordinary network APIs and undeclared subresources in script mode. Browser sandboxing cannot prevent a script from navigating its own frame, which can make a network request, and the warning states this limitation before scripts run. Script mode is never selected automatically.

Each valid feed route opens a scoped server-sent event stream. The daemon retains 256 live events and replays at most 100 durable posts after `Last-Event-ID`; lag or a larger replay emits `reset` so the browser reloads the latest page. Post events contain the complete API `Post` object and use the positive post ID as the SSE ID. Session closure emits `session-closed`. The browser deduplicates post IDs and orders posts by `published_at DESC, id DESC`.

At the top of the page, live posts enter the feed immediately. Away from the top, the browser retains at most 100 pending posts without changing the feed or viewport. The new-content control merges that queue and returns focus to the newest post. A larger burst switches to reconciliation instead of retaining more data. Session pages send a heartbeat every 30 seconds only while visible and while SSE is open. They also provide a confirmed close control that stops live work and releases renderer resources after successful deletion.

## CLI

Every short-lived CLI command writes one JSON value to standard output. Failures use the same output channel and exit nonzero. A committed publication remains successful if an explicitly requested browser launch fails; `result.browser_launch` reports the launch outcome. `GLIM_DAEMON_URL` overrides the development client endpoint; the default is `http://127.0.0.1:3030`. The Phase 2 client accepts an HTTP origin only. HTTPS transport remains deferred to Phase 4.

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
