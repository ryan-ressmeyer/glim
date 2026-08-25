# Compatibility policies

These policies apply to the interfaces implemented so far. CLI JSON commands remain pending.

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

The checked contract is `docs/openapi-v1.json`. It covers health, session resolution and lookup, post lookup and scoped listing, heartbeat, close, and Phase 2B multipart publication. Artifact-byte serving is not part of this contract.

All v1 JSON fields use `snake_case`. Request objects reject unknown fields. Errors use one envelope with a stable `error.code`, a human-readable `error.message`, and an object-valued `error.details`. Unknown v1 routes and unsupported methods use the same JSON envelope; malformed v1 paths, queries, and JSON bodies return enveloped `400` responses. Raw SQLite messages, internal paths, and debug representations are not API fields.

A visible-session heartbeat has no request body. The daemon records its wall-clock Unix time, so clients cannot set session activity to stale or future timestamps. The explicit-time storage operation remains a deterministic storage-test seam and is not exposed by HTTP.

Post lists use bounded reverse-publication ordering. The daemon orders posts by `published_at DESC, id DESC`; an exclusive `published_at:post_id` cursor continues after the final post in a page. The default page size is 20 and the maximum is 100. Clients must treat cursors as server-issued values even though the v1 encoding is documented.

Working directories returned with project metadata are identity values supplied during session resolution. The daemon does not read them as source paths. The compatibility `glim::app()` constructor exposes the complete route surface but returns `503 storage_unavailable` for stateful routes. Tests and configured embeddings use `glim::app_with_store(Store)`.

### Phase 2B publication and daemon additions

`POST /api/v1/posts` accepts streaming `multipart/form-data`. The first part is a UTF-8 JSON part named `manifest`, limited to 64 KiB. The manifest rejects unknown fields and declares at most 256 uniquely named byte parts. Every declared part must appear exactly once after the manifest; multipart arrival order does not affect visible-file or support-asset order. Stored filenames and support paths come from the manifest. Client content-disposition filenames, declared MIME values, working directories, and other host paths do not grant filesystem-read authority.

The daemon stages each byte part incrementally under the configured per-file upload limit. It resolves the project and session in the same immediate SQLite transaction that checks revisions and quota, finalizes blobs, and inserts the post. The response is `201 application/json` with `session` and `post` read models. Multipart parsing, validation, interruption, quota, revision, filesystem, and database failures use the v1 error envelope.

The runnable binary opens a persistent store before binding `127.0.0.1:3030`. Store-root selection uses `GLIM_STORE_ROOT` first as a development and test override, then nonblank `XDG_DATA_HOME/glim`, then `$HOME/.local/share/glim`. Startup fails when none is usable. Every leaf store root newly created by the daemon has mode `0700` on Linux, including a new explicit override. An existing explicit override retains its permissions. Phase 4 still owns final configuration precedence, production limits, binding options, authentication, and service management.

## CLI JSON schemas

- Canonical JSON input and structured output will carry an explicit schema version when they are introduced.
- Within one schema major version, required input fields and the meaning and type of existing output fields remain stable. Producers may add optional input fields; consumers must ignore unknown output fields.
- A breaking field or semantic change requires a new schema major version. Unsupported versions fail with a nonzero exit status and a parseable JSON error when JSON output is requested.
- Human-readable CLI output is not a machine interface. Automation must use the documented JSON input and output modes.
