# Release acceptance record

This record contains no credentials, public IDs, generated URLs, transcripts, or temporary paths.

## Live Pi matrix

| Model | Mode | Result |
| --- | --- | --- |
| `openai-codex/gpt-5.6-sol` | JSON | PASS (confirmed tool call) |
| `openai-codex/gpt-5.6-sol` | RPC | PASS (resumed revision and commands) |
| `openrouter/anthropic/claude-haiku-4.5` | print | PASS (confirmed tool call) |
| `openrouter/anthropic/claude-haiku-4.5` | RPC | PASS (resumed commands) |

Each passing publication row used an actual `glim_publish` model tool call with built-in tools disabled. RPC command checks covered `/glim-feed`, `/glim-status`, and `/glim-close` where the persisted model session was available. The harness permits one publication call and at most three model turns per path. Provider calls have a four-minute default deadline, combined subprocess output is capped at 2 MiB, and the complete run has a 30-minute default deadline.

## Artifact and browser coverage

Local install, package, daemon, and browser boundary: PASS

| Renderer family | Result |
| --- | --- |
| image | PASS |
| svg | PASS |
| markdown | PASS |
| text | PASS |
| json | PASS |
| csv | PASS |
| html | PASS |
| pdf | PASS |
| audio | PASS |
| video | PASS |

Session closure and purge: PASS

Fixtures covered Markdown with a collected image, SVG/image, text, JSON, CSV, HTML with collected CSS, PDF, audio, and video. Chromium used the token login form and left HTML scripts disabled. The browser check rejected runtime exceptions, required a live immutable revision before its predecessor, and required two exact Glimse session identities in one project feed.

## Deferred release criteria

A real release tag and downloaded GitHub release remain untested. A real user-service install also remains untested because acceptance must not alter user service state. The local archive reproduces the release archive layout and checksum procedure, but it does not replace those release criteria.

