---
name: glim
description: Use when a shell-capable agent creates visual or inspectable artifacts that benefit from browser inspection, or when a user asks to publish, show, revise, or inspect artifacts through Glimse.
compatibility: Requires a shell-capable agent and the Glimse CLI.
---

# Glimse artifact publication

Publish deliberate inspection results through Glimse. Suitable artifacts include plots, images, SVG, PDF, HTML, Markdown reports, media, inspection-oriented JSON or CSV, and related multi-file results.

Do not publish routine source changes, diffs, test output, logs, terminal transcripts, prose-only status messages, or files merely because they exist. Never watch directories or scan for artifacts automatically. Publish only files selected for this post. Every post needs at least one artifact and concise commentary that tells the user what to inspect.

Read the [CLI contract](references/cli-contract.md) when preparing a publication, interpreting output, handling an error, revising a post, opening a browser, or closing a session. It is the authority for fields, safe manifest construction, commands, envelopes, and stable error codes.

When the user requests a plan, or execution tools are unavailable, describe this same ordered workflow without claiming that commands ran. Show the intended health check, safely serialized canonical manifest, publication command, and output checks. Keep unresolved paths explicit rather than inventing them.

## Workflow

### Step 1: Select the inspection result

Confirm that the user requested Glimse or that one or more created artifacts would benefit from browser inspection. Select only the deliberate result files, preserve the user or agent's file order, and write a short title plus inspection-focused commentary. Captions are optional. If the available material is only routine implementation output, continue the task without publishing.

**Complete when:** either publication is excluded and work continues without a Glimse post, or an ordered nonempty artifact list, concise title, and concise inspection-focused commentary have been selected explicitly.

### Step 2: Establish availability and identity

Confirm that `glim` is on `PATH`, then run `glim health`. Parse the command's single JSON value and exit status; do not scrape human-readable text. Proceed only after a zero exit status and an envelope with `ok: true` whose health result reports `ok: true`.

Choose the harness namespace (`claude-code`, `codex`, `pi`, or a stable integration label). Reuse one readable external session key for this agent or conversation. Derive it from a stable public harness session ID when available. Otherwise ask the user, or create one collision-resistant per-conversation key once and retain it in conversation context. Do not write daemon session-token state. Resolve the absolute working directory and a concise project label. Reject secrets, inaccessible paths, and non-absolute source paths before publication.

If the binary or daemon is unavailable, report the stable code and the documented operator action from the CLI contract. Do not install, start, reconfigure, or change access settings without user approval.

**Complete when:** `glim` is available, health has a verified healthy success envelope, and the namespace, stable external key, absolute working directory, project label, and accessible non-secret absolute artifact paths are known; or publication has stopped with the exact failure code and a non-mutating recovery suggestion.

### Step 3: Publish canonical input

Construct one schema-versioned JSON document with a JSON serializer or structured file tool, write it to a temporary manifest, and redirect it to `glim publish --json`. Copy the flat top-level shape from [the publication fixture](assets/publication.json): `integration_namespace`, `external_session_key`, `project_label`, `working_directory`, `title`, `commentary`, optional `predecessor_post_id`, and `files` with `source_path`. Do not regroup these fields under invented `integration`, `project`, `post`, or `artifacts` objects. Never interpolate user-authored text into shell-constructed JSON. Include `--open` only when the user asked to open or inspect the result in a browser. Follow the safe construction and field rules in the CLI contract.

**Complete when:** one canonical manifest containing the stable identity, project context, concise post text, and every ordered artifact has been sent through standard input exactly once, and the CLI exit status plus its one JSON output value have been captured.

### Step 4: Interpret publication output

Require a success envelope with `ok: true` before reporting publication. Use only the returned post, session, `viewer_url`, `post_url`, and browser-launch fields; never construct URLs manually. A requested browser-launch failure does not erase a successful publication, so report the returned URL and the launch error separately.

For a rejection where `publication_may_have_succeeded` is absent or false, correct only a retryable cause and then retry. Do not retry a limit or classification rejection unchanged. When `publication_may_have_succeeded: true`, treat inspection as a mandatory barrier before another publication. Report that the post may exist, then inspect or list the current state with a confirmed session identity. If that identity or read path is unavailable, ask the user for it. Urgency and a direct request to retry do not bypass this barrier. Glimse provides no idempotency key.

**Complete when:** success is supported by `ok: true` and the returned identifiers and URLs have been reported, including any separate browser-launch outcome; or failure has been classified as a safe rejection or ambiguous publication, and no retry has occurred before the mandatory state inspection.

### Step 5: Publish a revision when requested

Create a new publication with `predecessor_post_id` set to the earlier positive post ID. Keep the same integration namespace, external session key, project label, and working directory. Revisions are immutable new posts; never mutate or replace the predecessor. Then apply Steps 3 and 4.

**Complete when:** the new canonical manifest carries the predecessor ID and unchanged identity and project context, and its outcome has met the publication-output completion criterion without changing the earlier post.

### Step 6: Close the current Glimse session when directed

Close only after explicit user direction or a known harness session-end hook. Use the current session's returned `result.session.public_id` with `glim close PUBLIC_ID`; never substitute an external key, post ID, agent exit, or guessed identity. If the current public ID is unavailable, inspect a safe list or status path or ask the user rather than guessing. Parse the close command's exit status and require `ok: true`. Explain that closing purges the ephemeral feed and snapshots.

**Complete when:** the confirmed current public session ID has an `ok: true` close envelope and the purge has been explained, or closure has stopped without affecting any session because identity or success could not be confirmed.
