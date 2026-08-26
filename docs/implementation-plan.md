# Implementation plan

## Delivery strategy

Build Glimse as a sequence of usable vertical slices. Each phase begins with failing tests for its public behavior, adds the minimum implementation needed to pass, and ends with fresh verification. Architectural helpers should emerge from tested behavior rather than precede it.

The placeholder binary on `main` marks the planning baseline. Feature work should use a dedicated branch or worktree.

## Phase 0: Repository and quality foundations

### Scope

- Choose a Rust workspace layout that separates reusable core logic from CLI, daemon, and pi-package concerns without creating speculative crates.
- Establish formatting, linting, unit-test, integration-test, and frontend-test commands.
- Add continuous integration for the supported Linux target.
- Add a reproducible frontend build whose output can be embedded in the Rust binary.
- Document local development commands and the supported Rust and Node versions.
- Define a migration policy for SQLite and a compatibility policy for the HTTP API and CLI JSON schemas.

### Exit criteria

- One command runs all Rust and frontend checks locally.
- CI runs the same checks from a clean checkout.
- The placeholder daemon can embed and serve a static frontend asset in a test fixture.
- No production session or publication behavior exists yet.

## Phase 1: Storage and ephemeral session lifecycle

### Scope

- Introduce configuration directories and an isolated test-store abstraction.
- Create the initial SQLite schema for integrations, projects, sessions, posts, files, revisions, and blob references.
- Implement schema migrations and startup recovery.
- Implement content-addressed blob writes using temporary files, hashing, atomic rename, and transactional references.
- Resolve sessions from integration namespace, external key, and project context.
- Allocate short public session IDs with collision-driven length growth.
- Implement explicit session close as an atomic metadata and blob purge.
- Implement seven-day inactivity calculation and a deterministic garbage-collection operation.
- Track publication activity and visible-viewer heartbeats without treating background reads as activity.
- Enforce a configurable per-file upload ceiling while streaming each staging file.
- Enforce one configurable global physical blob budget charged by unique finalized bytes. Deduplicated content adds no usage.
- Stage independent publication streams in lock-backed journals and atomically publish immutable posts, ordered visible files, nested support assets, occurrence references, revision links, publication timestamps, and session activity.
- Recover publication crashes from bounded journals without scanning finalized blobs. Remove uncommitted final links, retain committed referenced blobs, and leave active writers untouched.

### Test-first scenarios

- Concurrent session resolution returns one active session for the same identity.
- Distinct integrations may reuse the same external key without collision.
- Short-ID collisions extend identifiers without changing existing IDs.
- Duplicate content creates one blob and multiple references.
- A failed publication leaves no metadata, temporary file, or leaked blob.
- Closing one session retains blobs referenced by another session.
- Closing the final referencing session removes the blob and session record.
- Inactivity purge respects publication and visible-viewer activity.
- Per-file ceiling violations leave no staging file, finalized blob, or metadata.
- Concurrent unique blob finalization cannot overcommit the global budget.
- Startup recovery removes an uncommitted final file and staging journal when adopting it would exceed the configured global budget.
- Temporary staging remains bounded per upload but is outside finalized-store accounting; ordinary filesystem-full errors remain possible.
- Publication validation rejects blank titles or commentary, empty visible-file lists, invalid revision links, and duplicate support paths with stable storage errors.
- Aggregate publication quota checks count distinct new hashes once under the same serialized transaction as finalization and metadata insertion.
- Publication recovery distinguishes staged, pre-commit finalizing, and committed-before-cleanup states without adopting uncommitted publication blobs as standalone blobs.

### Exit criteria

- Storage and lifecycle tests pass against temporary real SQLite databases and filesystems.
- Purge is idempotent and safe after process interruption.
- The daemon does not retain artifact payloads in memory after a request completes.

## Phase 2: Versioned daemon API and CLI

### Phase 2A foundation completed

Phase 2A defines the checked OpenAPI 3.1 contract and implements storage read models plus stateful daemon endpoints for session resolution and lookup, scoped post listing, post lookup, visible-session heartbeat, and session close. Lists are bounded and cursor-based. Storage reads preserve immutable nested post data and do not refresh session activity. Heartbeats use daemon-owned wall-clock time. Every v1 routing and extraction failure uses the JSON error envelope. `app_with_store(Store)` runs synchronous SQLite operations on Tokio's blocking pool behind one shared mutex; `app()` retains the same frontend and health behavior while stateful routes return a stable unavailable error.

This foundation did not satisfy the Phase 2 exit criteria.

### Phase 2B streaming publication completed

Phase 2B wires the runnable daemon to a persistent per-user store and adds `POST /api/v1/posts`. The endpoint requires a bounded manifest-first multipart request, stages artifact chunks without holding the shared SQLite mutex, and cleans incomplete stages when parsing fails or a request is cancelled. Publication resolves or creates its project and session inside the same immediate transaction used for predecessor checks, quota enforcement, blob finalization, post insertion, and activity updates. The checked OpenAPI artifact documents the multipart contract, limits, response, and stable error codes.

### Phase 2C artifact classification and delivery completed

Phase 2C validates visible-file media declarations against manifest filenames and bounded byte prefixes. Schema v5 persists the effective media type and a closed renderer classification; older files migrate to the download fallback without scanning finalized bytes. Versioned `GET` and `HEAD` routes stream visible files and associated nested support assets from validated open blob handles after releasing the store mutex. The routes implement one RFC 9110 byte range, immutable caching, sanitized dispositions, nosniff, restrictive sandbox policy, exact association lookup, and traversal-resistant support paths.

### Phase 2D structured CLI and provenance completed

Phase 2D adds the canonical JSON and one-file publication interfaces, one-value JSON command output, scoped list/show/status/close/open commands, explicit browser launch, streaming file-handle multipart uploads, bounded Markdown and HTML dependency collection, and immutable optional Git provenance. The CLI uses `GLIM_DAEMON_URL` only as a development and test override. Authentication and final configuration precedence remain assigned to Phase 4.

### Scope

- Define versioned request, response, and error schemas for health, session resolution, publication, listing, revision lookup, heartbeat, and close.
- Implement authenticated and unauthenticated loopback API modes.
- Implement streaming multipart publication so the CLI uploads bytes rather than granting path access.
- Reject the whole publication when any upload exceeds the per-file ceiling or its additional unique finalized bytes exceed the global budget.
- [Completed in Phase 2C] Add content sniffing and extension/declaration validation.
- [Completed in Phase 2D] Implement minimal project and Git provenance collection in the CLI.
- [Completed in Phase 2D] Parse Markdown and HTML resource references in the CLI, collect contained allowlisted assets, and reject traversal or symlink escape.
- [Completed in Phase 2D] Implement canonical JSON-on-stdin publication and JSON output.
- [Completed in Phase 2D] Add ergonomic flags for a one-file post, commentary input, captions, revisions, and explicit browser opening.
- Ensure cancellation and connection loss clean up temporary uploads.

### Test-first scenarios

- CLI and API schema fixtures round-trip across supported versions.
- Multiline Markdown and ordered file captions survive publication unchanged.
- MIME mismatches return stable machine-readable errors.
- Local asset collection preserves safe relative paths and rejects every escape form.
- Multipart interruption leaves no visible post or orphaned temporary data.
- Concurrent uploads of identical bytes converge on one blob.
- Revision publication links immutable posts without modifying the predecessor.
- CLI JSON errors remain parseable and produce nonzero exit codes.

### Exit criteria

- A shell command can create a session-scoped post, list it, revise it, and close the session through the daemon.
- The API never accepts a source path as authority to read a host file.
- OpenAPI or an equivalent checked schema artifact documents the supported API.

## Phase 3: Live browser feed

### Phase 3 completed

The first Phase 3 slice serves routed session, project, and global feeds from the embedded frontend. It adds bounded cursor pagination, cached session provenance, scoped navigation, sanitized Markdown commentary, and safe static renderers for images, SVG, Markdown artifacts, text, JSON, CSV, and downloads. The feed remains intentionally fully expanded.

The rich-renderer slice adds native video and audio controls without autoplay. An observer pauses offscreen media and releases each source outside a 1,000-pixel vertical margin, then restores the source without starting playback. The bundled PDF.js renderer requests the artifact URL with 64 KiB range chunks and disabled eval support. It creates ordered placeholders for every page, materializes pages within a 1,500-pixel vertical margin, renders at feed width, and keeps at most three canvases per artifact with deterministic least-recently-used eviction. Disconnect and renderer replacement cancel page work, release media and canvas resources, destroy PDF loading tasks, and suppress expected cancellation rejections. Vite emits the bundled worker at `/assets/pdf.worker.mjs`, and Rust embeds that asset with the application script.

The HTML-renderer slice fetches each entry through its visible artifact route and parses it in a detached document. It removes untrusted base and policy elements, nested frames and plugins, active forms, and external navigation. Declarative resources resolve only through the file's listed support assets. HTML then renders in a unique-origin iframe with scripts disabled, no lifted sandbox tokens, and a deterministic content security policy. An explicit warning can reload the artifact with only `allow-scripts`. Connection APIs and undeclared subresources remain blocked, but a script can navigate its own frame and thereby make a network request. The warning discloses this browser-platform limitation. Disconnects, reconnects, and failures abort entry fetches and destroy iframe browsing contexts.

The final live-feed slice adds scoped SSE publication and closure events, 100-post durable replay, 256-event broadcast fan-out, and reset recovery for stale or lagging clients. Browser connections begin before the initial page load, validate events through the page contract, deduplicate post IDs, and order posts by publication time and ID. Existing renderer nodes move without recreation when ordering changes.

Top-of-page viewers receive posts immediately. Scrolled viewers retain at most 100 pending posts and use an accessible new-content control to merge them. Larger bursts trigger latest-page reconciliation. Session pages send 30-second heartbeats only while visible and connected to an open SSE stream. A confirmed close action stops SSE and heartbeat work, removes renderer resources, and reports retryable failures without displaying daemon output. Session closure events close matching session views and reconcile project or global views. These behaviors complete the Phase 3 scope and exit criteria.

### Scope

- Build the session feed with vanilla TypeScript and web components.
- Add project and global feed scope navigation.
- Implement SSE connection, reconnection, event ordering, and stale-event recovery.
- Insert new posts immediately when the viewport is at the top.
- Preserve scroll position and show a new-content indicator when the user has scrolled away.
- Render post titles, Markdown commentary, provenance, captions, revision links, and ordered files.
- Add visible-session heartbeat behavior using page visibility and connection state.
- Add explicit session close in the viewer with destructive confirmation.
- Add responsive layouts for desktop and remote mobile browsers.

### Renderer sequence

1. Images and SVG with natural sizing and zoom/pan.
2. Video and audio with controls, no autoplay, offscreen pause, and range requests.
3. Markdown with sanitized local-resource handling.
4. Raw text and highlighted code in resizable virtualized panes.
5. Structured JSON and CSV panes.
6. PDF.js page-by-page rendering with lazy materialization.
7. Sandboxed HTML with scripts disabled by default, explicit `allow-scripts` opt-in, unique origin, and a restrictive CSP.
8. Download fallback for unsupported files.

### Test-first scenarios

- Feed insertion preserves the viewport in both top and scrolled states.
- SSE reconnect fills missed posts without duplicates or reordering.
- Hidden or disconnected pages do not keep a session active.
- Offscreen media pauses and releases bounded resources.
- Range responses handle valid, invalid, suffix, and partial requests.
- Malicious Markdown and SVG cannot access viewer credentials, other posts, or the network.
- HTML has no viewer origin privilege; its default mode blocks scripts and network access, while explicit script mode blocks ordinary network APIs and discloses self-navigation egress.
- PDF, text, JSON, and CSV renderers remain usable within configured file limits.
- Keyboard and touch interactions work for resizing, fullscreen, zoom, and the new-content indicator.

### Exit criteria

- Two concurrent agent sessions publish into isolated live feeds.
- A remote browser can inspect every supported renderer without opening host applications.
- Browser memory remains bounded while scrolling through a representative media-heavy feed.

## Phase 4: Network configuration and service lifecycle

### Configuration and safe-listener slice completed

The daemon now reads a strict, versioned JSON configuration from the XDG or home configuration directory, with an explicit `GLIM_CONFIG` override. Files are bounded to 64 KiB. Environment values override file values, and file values override secure defaults. `GLIM_STORE_ROOT` and `GLIM_BIND` are the initial environment controls.

The listener accepts numeric loopback addresses with nonzero ports. Non-loopback values fail before the store opens or a socket binds. This preserves the local-only security boundary while allowing isolated ports for development and tests.

### Direct token authentication slice completed

Token mode generates or loads a persistent 256-bit credential from a strict mode-`0600` non-symlink file. Non-loopback token mode requires operator-provided PEM TLS material and an HTTPS public origin whose port matches the listener. The CLI supports HTTPS and sends the configured token as a Bearer credential.

One router-wide middleware protects feed pages, API routes, SSE, range requests, and artifact delivery. The browser exchanges the persistent token for a bounded 12-hour HttpOnly `SameSite=Strict` session. Cookie mutations require the configured exact origin. Logout invalidates server state and expires the cookie. HTML iframes receive renewable five-minute capabilities scoped to one post and file support subtree, preserving unique-origin sandboxing without exposing the session cookie. Service commands, status expansion, and structured logs remain in later Phase 4 slices.

### Trusted-proxy access slice completed

Trusted-proxy mode accepts a nonempty allowlist of exact numeric proxy IP addresses and a canonical public origin. Router middleware authorizes the immediate TCP peer supplied by the listener and ignores all forwarded identity, host, and scheme headers. Health remains public; every other route requires an allowlisted peer. Untrusted API responses use a stable sanitized error, pages fail without a login redirect, and token session endpoints are unavailable. Browser-originated mutations require the exact configured origin, while headerless non-browser API clients rely on peer authorization.

Configuration files and environment overrides preserve environment-over-file precedence and reject token or daemon-TLS settings in trusted-proxy mode. Non-loopback listeners require an HTTPS public origin but serve plaintext to the proxy. Real-socket tests cover ordinary API access, SSE, range delivery, and untrusted-peer denial. Specific reverse-proxy products and Tailscale Serve configurations remain operator-managed boundaries rather than tested Glimse integrations.

### Scope

- Define configuration-file and environment-variable precedence.
- Bind to loopback by default.
- Generate and securely store a persistent access token when configured for direct non-loopback access.
- Implement trusted-proxy mode with explicit allowed proxy settings.
- Protect HTML pages, SSE, API routes, media, and downloads consistently.
- Add CSRF and origin protections appropriate to cookie or bearer-token handling.
- Implement `service install`, `start`, `stop`, `status`, and `uninstall` for a systemd user unit.
- Add daemon health, version, store-size, active-session, and cleanup status commands.
- Add structured logs with bounded verbosity and no artifact content or tokens.

### Test-first scenarios

- Default startup is unreachable from non-loopback interfaces.
- Non-loopback startup fails closed without token or trusted-proxy configuration.
- Authentication covers SSE and range requests as well as ordinary pages and API calls.
- Proxy headers are ignored outside trusted-proxy mode.
- Service commands are idempotent and preserve user configuration and session data unless purge is explicit.
- Logs redact credentials, commentary, filenames where necessary, and request bodies.

### Exit criteria

- A fresh Linux user can install and operate the daemon without writing a unit file manually.
- SSH forwarding, Tailscale Serve, and direct authenticated binding have documented configurations.
- Security-focused integration tests pass through a real listening socket.

## Phase 5: Agent integrations

### Generic skill

- Teach shell-capable agents when visual output warrants publication.
- Require an artifact and concise commentary rather than text-only feed messages.
- Use canonical JSON stdin to avoid shell-quoting failures.
- Derive or request a readable external key without storing daemon session tokens.
- Explain revisions, session closure, limits, and returned links.
- Avoid automatic publication of routine source changes or terminal output.

### Native pi extension

- Package a thin extension from the same repository.
- Register a typed publication tool with ordered files, captions, commentary, and revision target.
- Supply pi session and workspace provenance from public extension APIs.
- Register commands to show the current feed URL, report daemon status, and close the session.
- Use the daemon API or CLI client library without duplicating storage or rendering logic.
- Fail clearly when the `glim` daemon or compatible binary is unavailable.

### Test-first scenarios

- Skill examples produce schema-valid CLI input.
- The pi tool maps extension arguments to the same API model as the CLI.
- Session identity remains stable across repeated calls and isolated across pi sessions.
- Integration errors do not imply that a post was published when the daemon rejected it.
- Closing through pi purges the corresponding feed and no other session.

### Exit criteria

- Claude Code can publish through the generic skill.
- pi can publish through its native tool without using a shell command.
- Both integrations can contribute to separate sessions in one daemon concurrently.

## Phase 6: Hardening and release

### Scope

- Add fuzz or property tests for path handling, MIME detection, range parsing, short IDs, and schema decoding.
- Exercise crash recovery around SQLite commits and blob renames.
- Benchmark hashing, concurrent upload, feed queries, garbage collection, and media serving.
- Verify bounded memory with large allowed files and long feeds.
- Add browser security tests for sandbox escape and cross-post access.
- Produce Linux release binaries and checksums in CI.
- Support `cargo install` for developer installation.
- Document upgrades, database migrations, backup expectations, and complete removal.
- Run the native pi package through pi's package-loading and reload lifecycle.

### Release criteria

- A clean machine can install a release binary, install the user service, publish through CLI and pi, inspect remotely, close the session, and confirm complete purge.
- Supported upgrade paths preserve active sessions or fail before migration without corrupting them.
- Security tests cover every content renderer and authenticated route.
- Release artifacts are reproducible enough to trace each binary to a tagged commit and published checksum.

## Decisions deferred until their owning phase

The implementation should choose these only when a failing test or phase requirement makes them concrete:

- Rust crate boundaries and third-party crates;
- exact SQL tables and indexes;
- API paths and JSON field names;
- production default byte values for the global physical blob budget, per-file upload ceiling, and browser virtualization thresholds;
- visual theme and component styling;
- Base58 versus Base62 public IDs;
- token transport details;
- frontend test runner and browser automation library.

Deferring these choices prevents the planning scaffold from becoming untested architecture.
