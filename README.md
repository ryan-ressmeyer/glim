# Glimse

Glimse is a local service for visual output produced by terminal-based AI agents. The Phase 2B daemon listens on loopback and persists metadata and uploaded bytes under a per-user store root. It implements session resolution and lookup, scoped post listing and lookup, visible-session heartbeats, session close, and streaming multipart publication through the versioned HTTP API.

The API is usable directly with HTTP clients such as `curl`. The checked contract is [`docs/openapi-v1.json`](docs/openapi-v1.json). A CLI, artifact-byte serving, MIME validation, the live viewer and media renderers, authentication, and service management remain pending. The embedded frontend is still a foundation; published artifacts are not browser-viewable yet.

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

Clippy runs with warnings denied. Cargo commands use `Cargo.lock`, and frontend installation uses `web/package-lock.json`. Run the daemon after building.

```bash
make build
cargo run --locked
```

The daemon listens on `127.0.0.1:3030`. It selects the store root from `GLIM_STORE_ROOT`, then `$XDG_DATA_HOME/glim`, then `$HOME/.local/share/glim`. Use an absolute `GLIM_STORE_ROOT` for an isolated development store.

```bash
GLIM_STORE_ROOT=/tmp/glim-store cargo run --locked
curl http://127.0.0.1:3030/api/v1/health
```

The frontend build writes `web/dist/index.html` and `web/dist/assets/app.js`. Rust embeds both files with `include_str!` and `include_bytes!`, so the resulting binary does not read frontend files at runtime.

## Compatibility

[`docs/compatibility.md`](docs/compatibility.md) defines the SQLite migration policy, HTTP API versioning policy, and CLI JSON schema policy.

## License

Glimse is available under the [MIT License](LICENSE).
