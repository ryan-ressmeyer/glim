# Compatibility policies

These policies apply to the SQLite store, v1 HTTP API, and v1 CLI JSON interfaces.

## SQLite migrations

- Every released schema change is a numbered, forward-only migration.
- A released migration is immutable: later releases add a new migration rather than editing or reordering an existing one.
- Startup applies pending migrations transactionally and refuses to open a database whose schema version is newer than the binary understands.
- Releases document the oldest schema version they can upgrade. A migration that cannot preserve supported data requires an explicit export or backup procedure before release.
- Downgrades are not supported unless a release explicitly documents them. Users must back up the database before moving to an older binary.

## HTTP API

- The major API version is part of the path, for example `/api/v1/health`.
- Within a major version, existing request meanings, response fields, status codes, and machine-readable error identifiers remain compatible. New optional fields or endpoints may be added, and clients must ignore unknown response fields.
- Removing or renaming a field, changing its type or meaning, or otherwise requiring client changes introduces a new major API path. Old and new major versions receive a documented overlap period before removal.
- The health response in v1 keeps `ok` as a boolean and `version` as a string.

### Current v1 contract

The checked contract is `docs/openapi-v1.json`. It covers health, session resolution and lookup, post lookup and scoped listing, heartbeat, close, multipart publication, artifact classification, and associated artifact-byte delivery.

All v1 JSON fields use `snake_case`. Request objects reject unknown fields. Errors use one envelope with a stable `error.code`, a human-readable `error.message`, and an object-valued `error.details`. Unknown v1 routes and unsupported methods use the same JSON envelope; malformed v1 paths, queries, and JSON bodies return enveloped `400` responses. Raw SQLite messages, internal paths, and debug representations are not API fields.

When token mode is enabled, the health endpoint, login exchange, login page, and compiled assets remain public. Every other page and v1 route requires an exact Bearer token or a valid browser session cookie. This includes unknown API paths, SSE, range requests, visible artifacts, and support assets. `POST /api/v1/auth/session` exchanges the persistent token for a browser cookie; `DELETE /api/v1/auth/session` invalidates it. Cookie-authenticated mutations require the configured exact origin. `POST /api/v1/posts/{post_id}/files/{position}/html-capability` issues a renewable scoped support prefix only for an existing HTML renderer. Local mode returns the ordinary support prefix from the same endpoint.

Trusted-proxy mode leaves only health public. Every other page, API, SSE, range, artifact, and download request requires an immediate TCP peer IP in the configured exact-IP allowlist. Untrusted API requests return `403 proxy_authorization_required`; untrusted pages return `403` without redirecting to token login. Token login and session endpoints return not found. Browser-originated state changes require an exact configured `Origin`; headerless non-browser clients rely on peer authorization. `GET`, `HEAD`, and `OPTIONS` are origin-exempt. The daemon never derives trust from forwarded headers.

A visible-session heartbeat has no request body. The daemon records its wall-clock Unix time, so clients cannot set session activity to stale or future timestamps. The explicit-time storage operation remains a deterministic storage-test seam and is not exposed by HTTP.

Post lists use bounded reverse-publication ordering. The daemon orders posts by `published_at DESC, id DESC`; an exclusive `published_at:post_id` cursor continues after the final post in a page. The default page size is 20 and the maximum is 100. Clients must treat cursors as server-issued values even though the v1 encoding is documented.

Working directories returned with project metadata are identity values supplied during session resolution. The daemon does not read them as source paths. The compatibility `glim::app()` constructor exposes the complete route surface but returns `503 storage_unavailable` for stateful routes. Tests and configured embeddings use `glim::app_with_store(Store)`.

### Phase 2B through 2D publication and daemon additions

`POST /api/v1/posts` accepts streaming `multipart/form-data`. The first part is a UTF-8 JSON part named `manifest`, limited to 64 KiB. The manifest rejects unknown fields and declares at most 256 uniquely named byte parts. Every declared part must appear exactly once after the manifest; multipart arrival order does not affect visible-file or support-asset order. Stored filenames and support paths come from the manifest. Client content-disposition filenames, declared MIME values, working directories, and other host paths do not grant filesystem-read authority.

The daemon stages each byte part incrementally under the configured per-file upload limit. It resolves the project and session in the same immediate SQLite transaction that checks revisions and quota, finalizes blobs, and inserts the post. The response is `201 application/json` with `session` and `post` read models. Multipart parsing, validation, interruption, quota, revision, filesystem, and database failures use the v1 error envelope.

Visible-file manifest entries may include `media_type`. The daemon validates exact media subtypes against the manifest filename and a bounded prefix of the staged bytes. Container signatures require supported brands or codecs; `M4A ` and `M4B ` ISO-BMFF brands map to `audio/mp4` for `.m4a` and `.m4b` files. A truncated prefix may omit only an incomplete final UTF-8 code point. Bounded JSON validation accepts an incomplete prefix only when the parser reaches the end of that prefix without an earlier syntax error. Markdown, CSV, and HTML rely on their extension plus bounded valid UTF-8 because valid fragments and quoted fields do not admit a reliable prefix grammar check. Persisted `media_type` and closed `renderer` values let clients select a renderer without browser sniffing. Schema v5 assigns pre-v5 files `application/octet-stream` and `download` without reading historical blobs.

Visible bytes use `GET` or `HEAD /api/v1/posts/{post_id}/files/{position}/content`. Support bytes use `GET` or `HEAD /api/v1/posts/{post_id}/files/{position}/support/{asset_path}` and resolve through the exact post, visible-file position, and stored relative path. Bounded support classification supplies safe types for validated CSS, JavaScript, JSON, supported media, SVG, Wasm, and common browser fonts. Encoded separators, traversal segments, duplicate slashes, control characters, and malformed paths cannot alias another support asset. Missing associations return `artifact_not_found` without exposing blob hashes or filesystem paths.

Artifact responses include an effective content type, exact length, `Accept-Ranges: bytes`, a sanitized content disposition, private immutable caching, `X-Content-Type-Options: nosniff`, and a `default-src 'none'; sandbox` content security policy. One prefix, bounded, open-ended, or suffix byte range returns `206`. Malformed, multiple, and unsatisfiable ranges return `416`, `Content-Range: bytes */<length>`, and an empty body. The daemon opens and validates the final blob on the blocking pool, releases the store mutex, and then streams from the open handle.

Schema v6 adds nullable Git root, branch, and commit columns to immutable posts without replacing the production immutability trigger. Existing posts migrate with all three values absent. HTTP clients may supply an absolute control-free root, an optional nonblank control-free branch, and an optional full 40- or 64-digit hexadecimal object ID as bounded inert metadata. The daemon does not execute Git, inspect the supplied root, or collect repository state.

The runnable binary reads at most 64 KiB from a version 1 JSON configuration. It uses `GLIM_CONFIG` when set. Otherwise it uses `$XDG_CONFIG_HOME/glim/config.json` when `XDG_CONFIG_HOME` is usable, or `$HOME/.config/glim/config.json` as the fallback location. A missing default file is equivalent to an empty configuration, while a missing explicit file fails startup. Unknown fields, unsupported schema versions, malformed JSON, non-file paths, and oversized files fail startup.

Configuration precedence is environment value, file value, then default. `GLIM_STORE_ROOT` overrides `store_root`; the fallback remains nonblank `XDG_DATA_HOME/glim`, then `$HOME/.local/share/glim`. `GLIM_BIND` overrides `bind`, whose default is `127.0.0.1:3030`. Local mode requires a numeric loopback socket address with a nonzero port. Every newly created leaf store root has mode `0700` on Linux. An existing explicit store root retains its permissions.

Token mode requires an absolute token path and an HTTP or HTTPS public origin whose port matches the listener. Non-loopback token mode also requires an HTTPS origin and absolute PEM certificate and private-key paths. `GLIM_ACCESS_MODE`, `GLIM_TOKEN_FILE`, `GLIM_PUBLIC_ORIGIN`, `GLIM_TLS_CERTIFICATE`, and `GLIM_TLS_PRIVATE_KEY` override file values before final bind validation. The daemon generates a missing 256-bit token and persists its 64-character lowercase hexadecimal encoding with mode `0600` on Linux. Token reads use one non-symlink descriptor and reject malformed or group- or world-accessible files. TLS material and the token are loaded before the store opens.

Token mode leaves health, the login shell, and compiled frontend assets public. Bearer credentials authorize non-browser clients. Browser login issues a random in-memory session with a 12-hour lifetime and an HttpOnly `SameSite=Strict` cookie; TLS mode also marks the cookie `Secure`. Cookie-authenticated mutations require the configured exact `Origin`. Logout removes server-side session state. The browser requests renewable five-minute capabilities for HTML rendering. Each capability grants only `GET` and `HEAD` access to one post/file support subtree, so a unique-origin iframe does not receive the persistent token or browser cookie.

Trusted-proxy mode requires one or more exact numeric IPv4 or IPv6 addresses and a canonical HTTP or HTTPS public origin. `GLIM_TRUSTED_PROXY_IPS` is a comma-separated exact-IP override. Token and TLS settings are rejected in trusted-proxy mode, and proxy settings are rejected in local and token modes. Non-loopback trusted-proxy binds require an HTTPS public origin but do not require daemon-side TLS. The reverse proxy owns TLS and user authentication, while network controls must prevent non-proxy clients from reaching the listener. Glimse validates the socket peer supplied by the server connection and ignores all forwarded identity, host, and scheme headers.

## CLI JSON schemas

The checked v1 artifacts are `docs/cli-publish-v1.schema.json` and `docs/cli-output-v1.schema.json`. `glim publish --json` rejects unknown input fields and unsupported versions before opening an HTTP request. Every short-lived command writes exactly one v1 output value and uses a nonzero status for errors. Publication success requires a decoded `201` daemon response. After decoding a committed publication, browser-launch failure is reported inside the successful result rather than changing the command status. A malformed `201` error sets `publication_may_have_succeeded` so clients do not retry blindly.

Within one schema major version, required input fields and the meaning and type of existing output fields remain stable. Producers may add optional input fields only when older clients can reject them safely; consumers must ignore unknown output fields. A breaking field or semantic change requires a new schema major version.

Source paths exist only in CLI input. The CLI resolves and opens each source, then streams open handles as multipart byte parts. Daemon manifests contain filenames, ordered support paths, publication identity, and inert provenance, but no path that grants host filesystem authority.

Linux service commands use the same v1 output envelope as other short-lived commands. `glim service install` writes the marked `glim.service` unit under the absolute `XDG_CONFIG_HOME/systemd/user` directory, or under `$HOME/.config/systemd/user` when the XDG value is unusable. It reloads the systemd user manager and enables the unit without starting it. Start and stop require the marked unit. Status reports absent, inactive, and active states as successful structured results from a bounded `systemctl show` query. Uninstall stops and disables the service, removes only the marked unit, and reloads the user manager. It does not remove configuration, credentials, TLS files, stores, or sessions. Every command refuses to alter an unmarked `glim.service`; subprocess failures use stable service error codes without exposing unbounded command output. These commands require Linux and a functioning systemd user manager.

Markdown, HTML, and recursive linked-CSS dependency collection is bounded to 512 parsed references, eight CSS levels, 255 support assets per entry, and 256 total multipart byte parts. Text entries have no CLI-specific byte ceiling. The parsers require complete UTF-8 input, so the CLI temporarily holds parser text while retaining only discovered references afterward; upload bytes and daemon ingestion remain streamed. First-use depth-first order is stable. Local paths are percent-decoded once, normalized to slash-separated relative paths, contained by canonical entry-directory paths, and opened before the request begins. Remote schemes, protocol-relative URLs, fragments, and data or blob URLs are never fetched. Inline `<style>` content and `style` attributes remain deferred to the renderer slice.
