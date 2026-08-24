# Compatibility policies

These policies apply when the corresponding interfaces are introduced. Phase 0 has no SQLite schema or CLI JSON command yet.

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

## CLI JSON schemas

- Canonical JSON input and structured output will carry an explicit schema version when they are introduced.
- Within one schema major version, required input fields and the meaning and type of existing output fields remain stable. Producers may add optional input fields; consumers must ignore unknown output fields.
- A breaking field or semantic change requires a new schema major version. Unsupported versions fail with a nonzero exit status and a parseable JSON error when JSON output is requested.
- Human-readable CLI output is not a machine interface. Automation must use the documented JSON input and output modes.
