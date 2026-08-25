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

The runnable binary opens a persistent store before binding `127.0.0.1:3030`. Store-root selection uses `GLIM_STORE_ROOT` first as a development and test override, then nonblank `XDG_DATA_HOME/glim`, then `$HOME/.local/share/glim`. Startup fails when none is usable. Every leaf store root newly created by the daemon has mode `0700` on Linux, including a new explicit override. An existing explicit override retains its permissions. Phase 4 still owns final configuration precedence, production limits, binding options, authentication, and service management.

## CLI JSON schemas

The checked v1 artifacts are `docs/cli-publish-v1.schema.json` and `docs/cli-output-v1.schema.json`. `glim publish --json` rejects unknown input fields and unsupported versions before opening an HTTP request. Every short-lived command writes exactly one v1 output value and uses a nonzero status for errors. Publication success requires a decoded `201` daemon response. After decoding a committed publication, browser-launch failure is reported inside the successful result rather than changing the command status. A malformed `201` error sets `publication_may_have_succeeded` so clients do not retry blindly.

Within one schema major version, required input fields and the meaning and type of existing output fields remain stable. Producers may add optional input fields only when older clients can reject them safely; consumers must ignore unknown output fields. A breaking field or semantic change requires a new schema major version.

Source paths exist only in CLI input. The CLI resolves and opens each source, then streams open handles as multipart byte parts. Daemon manifests contain filenames, ordered support paths, publication identity, and inert provenance, but no path that grants host filesystem authority.

Markdown, HTML, and recursive linked-CSS dependency collection is bounded to 512 parsed references, eight CSS levels, 255 support assets per entry, and 256 total multipart byte parts. Text entries have no CLI-specific byte ceiling. The parsers require complete UTF-8 input, so the CLI temporarily holds parser text while retaining only discovered references afterward; upload bytes and daemon ingestion remain streamed. First-use depth-first order is stable. Local paths are percent-decoded once, normalized to slash-separated relative paths, contained by canonical entry-directory paths, and opened before the request begins. Remote schemes, protocol-relative URLs, fragments, and data or blob URLs are never fetched. Inline `<style>` content and `style` attributes remain deferred to the renderer slice.
