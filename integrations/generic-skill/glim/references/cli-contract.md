# Glimse CLI contract

## Contents

- [Canonical publication input](#canonical-publication-input)
- [Safe manifest construction](#safe-manifest-construction)
- [Publication output](#publication-output)
- [Failures and retry decisions](#failures-and-retry-decisions)
- [Revision](#revision)
- [Read, open, and close commands](#read-open-and-close-commands)

## Canonical publication input

`glim publish --json` reads one JSON document from standard input. The document follows `docs/cli-publish-v1.schema.json` in the Glimse repository.

| Field | Meaning |
| --- | --- |
| `schema_version` | Integer `1`. |
| `integration_namespace` | Stable harness identity such as `claude-code`, `codex`, or `pi`. |
| `external_session_key` | Readable key reused for the current agent or conversation session. It is not a daemon token or public session ID. |
| `project_label` | Concise human-readable project name. |
| `working_directory` | Absolute current project or analysis directory. |
| `title` | Concise description of the result. |
| `commentary` | Nonempty Markdown telling the user what to inspect. |
| `predecessor_post_id` | Optional positive post ID for an immutable revision. |
| `files` | One to 256 ordered visible artifacts. |

Each file requires an absolute `source_path`. Optional fields are `published_filename`, `caption`, `media_type`, and `collect_assets`. `collect_assets` defaults to true and gathers safe local dependencies referenced by Markdown or HTML. Preserve the intended visible-file order. Never include a secret, inaccessible file, directory, or file selected only because it exists.

Repository-owned schema-valid examples are [publication.json](../assets/publication.json) and [revision.json](../assets/revision.json). Replace their example values through a serializer or structured file tool rather than shell substitution.

## Safe manifest construction

Use the harness's JSON serializer or structured file tool to write a temporary file. A serializer must receive title, commentary, captions, paths, and identity as data values, not as fragments of JSON source. Confirm all paths are absolute before running the command.

```sh
manifest=$(mktemp)
trap 'rm -f "$manifest"' EXIT
# Write one JSON document to "$manifest" with a JSON serializer or structured file tool.
glim publish --json < "$manifest"
```

Add `--open` only for an explicit browser-inspection request.

```sh
glim publish --json --open < "$manifest"
```

A quoted heredoc is acceptable only for a fixed literal example with no user-authored or variable content. Do not build real manifests with `echo`, string concatenation, an unquoted heredoc, or shell interpolation. Glimse does not support an idempotency key.

## Publication output

Every short-lived command writes exactly one JSON value to stdout. A success exits zero and has this outer envelope:

```json
{
  "schema_version": 1,
  "ok": true,
  "result": {}
}
```

A successful publication result contains the created `session`, immutable `post`, `viewer_url`, `post_url`, and `browser_launch`. Treat `result.session.public_id` as the only close identity. Treat `result.post.id` as the post identity and future revision target. Use `viewer_url` and `post_url` exactly as returned; do not derive them from daemon configuration.

```json
{
  "schema_version": 1,
  "ok": true,
  "result": {
    "session": {"public_id": "7Yp2Qa"},
    "post": {"id": 48, "predecessor_post_id": null},
    "viewer_url": "http://127.0.0.1:3030/sessions/7Yp2Qa",
    "post_url": "http://127.0.0.1:3030/sessions/7Yp2Qa#post-48",
    "browser_launch": {"requested": false, "opened": false, "error": null}
  }
}
```

The abbreviated nested objects above illustrate the fields agents consume; the daemon returns complete session and post objects. Publication is successful only when the process exits zero and the envelope has `ok: true`. When `browser_launch.requested` is true but `opened` is false, the post remains successful. Report the browser error and returned `viewer_url` separately.

A failure exits nonzero:

```json
{
  "schema_version": 1,
  "ok": false,
  "error": {
    "code": "storage_limit_exceeded",
    "message": "Storage budget would be exceeded",
    "details": {"http_status": 507}
  }
}
```

## Failures and retry decisions

Parse `error.code`, `error.details`, and the process exit status. Do not infer success from command completion or prose.

| Condition | Stable code or signal | Required response |
| --- | --- | --- |
| Binary absent | Shell command-not-found status | Report that `glim` is missing. Ask before installation or reconfiguration. |
| Daemon unavailable | `daemon_unavailable` | Report the code. Suggest `glim service status` and, if the user wants the managed service started, `glim service start`; do not run a service command without approval. |
| Invalid local daemon or credential configuration | `configuration_error` | Report the configuration problem without changing access settings or token files. |
| Authentication needed or rejected | `authentication_required`, `invalid_credentials` | Ask the user to correct configured access. Never print or publish a token. |
| Local manifest or path problem | `validation_error`, `asset_collection_error`, `invalid_publication_json`, `filesystem_error` | Correct the manifest or selected path, then submit a new attempt. No request was accepted by the daemon. |
| Per-file upload limit | `upload_limit_exceeded` | Reduce or deliberately replace the artifact. Do not retry unchanged. |
| Finalized storage budget | `storage_limit_exceeded` | Ask the user to free space or select a smaller result. Do not retry unchanged. |
| Bytes contradict the filename or media type | `artifact_classification_failed` | Correct the content, filename, or declared media type. Do not retry unchanged. |
| Cross-session predecessor | `revision_conflict` | Confirm the predecessor and stable session identity. Do not move the revision to another session silently. |
| Response malformed after upload | `malformed_daemon_response` with `publication_may_have_succeeded: true` | State that the post may exist. Inspect or list confirmed state before any retry. |
| Transport/read ambiguity after upload | `daemon_unavailable`, `http_error`, or `daemon_response_too_large` with `publication_may_have_succeeded: true` | State that the post may exist. Inspect or list confirmed state before any retry. |
| Browser command failed | `browser_launch_failed`, either as command failure or under successful publication `browser_launch.error` | Keep a confirmed successful publication visible and provide its returned URL. Retry browser launch only if requested. |

A daemon rejection has `publication_may_have_succeeded` absent or false. It is safe to correct its cause before another attempt. A transport or response ambiguity has `publication_may_have_succeeded: true`; the server may already have committed the immutable post. Glimse has no idempotent replay mechanism, so confirmed state inspection must precede any new publication. A retry request does not remove this barrier.

`glim health` is the minimal public preflight. A healthy command exits zero and wraps a result shaped like `{"ok":true,"version":"..."}` in the standard success envelope. Use `glim status` only when authenticated storage and cleanup state is needed.

## Revision

A revision is a new canonical publication. Set `predecessor_post_id` to the earlier positive post ID and retain the same `integration_namespace`, `external_session_key`, `project_label`, and `working_directory`. The [revision fixture](../assets/revision.json) demonstrates predecessor `48` and external key `pi-run-77`.

Write the revised manifest safely, then use the same stdin contract:

```sh
glim publish --json < "$manifest"
```

A successful response returns a new `result.post.id`; its `result.post.predecessor_post_id` identifies the earlier immutable post.

## Read, open, and close commands

The supported structured commands are:

```sh
glim health
glim status
glim list --session PUBLIC_SESSION_ID --limit 20
glim list --project PROJECT_ID
glim list --global
glim show POST_ID
glim open PUBLIC_SESSION_ID
glim close PUBLIC_SESSION_ID
glim service status
```

Use `glim open` only after an explicit browser request. Prefer the exact returned `viewer_url` when available; `glim open` accepts either the public session ID or the returned session URL.

For closure, obtain `PUBLIC_SESSION_ID` from the current publication's `result.session.public_id`, not from `external_session_key`. After confirmed explicit direction or a known harness session-end hook:

```sh
PUBLIC_SESSION_ID=7Yp2Qa  # value previously returned as result.session.public_id
glim close $PUBLIC_SESSION_ID
```

Require a zero exit and `ok: true`. Closing deletes the session's ephemeral feed, posts, snapshots, and blobs no longer referenced elsewhere. If the current public ID is unavailable, use a known safe read path such as `glim list --session CONFIRMED_PUBLIC_ID` or ask the user. Never guess, close another session, or interpret exiting the agent as a Glimse close request.
