# Glimse

Glimse is an ephemeral browser feed for visual output produced by terminal-based AI agents. Agents will publish images, PDFs, video, audio, rendered documents, and structured text through the `glim` command. A local web service will present those results with the agent's commentary in a session-scoped feed.

Phase 0 provides the HTTP application and embedded frontend foundation. `GET /api/v1/health` reports the package version, and `/` serves the compiled Glimse web component from the binary. Sessions, publication, storage, authentication, and renderers remain deferred to later phases.

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

Clippy runs with warnings denied. Cargo commands use `Cargo.lock`, and frontend installation uses `web/package-lock.json`. Run the development server after building.

```bash
make build
cargo run --locked
```

The Phase 0 binary listens on `127.0.0.1:3030`. Network configuration belongs to a later phase.

The frontend build writes `web/dist/index.html` and `web/dist/assets/app.js`. Rust embeds both files with `include_str!` and `include_bytes!`, so the resulting binary does not read frontend files at runtime.

## Compatibility

[`docs/compatibility.md`](docs/compatibility.md) defines the SQLite migration policy, HTTP API versioning policy, and CLI JSON schema policy.

## License

Glimse is available under the [MIT License](LICENSE).
