# Product design

## Purpose

Glimse gives terminal-based AI agents a browser surface for results that do not render well in a terminal. An agent explicitly publishes one or more files with Markdown commentary. The user inspects the complete result in a live feed and returns to the terminal to continue the conversation.

Glimse is a temporary review surface. It is not an archive, project file browser, chat interface, or cloud publishing service.

## System boundary

The project has three integration layers in one repository.

1. A standalone `glim` CLI is the canonical interface for users and shell-capable agents.
2. A generic skill teaches Claude Code and other agents to call the CLI.
3. A thin pi extension registers typed publication and session commands and supplies pi provenance automatically.

The CLI and pi extension use the same versioned daemon HTTP API. Neither integration owns a separate store or viewer.

## Runtime architecture

- A persistent per-user daemon serves the API and web application.
- Rust implements the daemon and CLI.
- Vanilla TypeScript and web components implement the browser interface.
- Frontend assets are compiled and embedded in the release binary.
- SQLite in WAL mode stores sessions, posts, revisions, blob references, and provenance.
- A content-addressed filesystem store holds uploaded bytes.
- Server-Sent Events notify browsers about new posts and session lifecycle changes.
- A systemd user unit manages the daemon on Linux.

The daemon keeps metadata on disk, streams media from storage, and bounds all in-memory caches. Persistent service lifetime must not imply persistent artifact state in memory.

## Network and authentication

The daemon binds to loopback by default. Users may configure another interface.

Non-loopback deployments require one of two explicit modes.

- Built-in token authentication uses a generated persistent token.
- Trusted-proxy mode delegates authentication to a user-configured proxy or private network boundary.

The default configuration must not expose artifacts to the local network. Media endpoints support HTTP range requests for seeking and partial transfer.

## Sessions

A session is an ephemeral feed for one agent workflow.

Every publication identifies an integration namespace, an external session key, and project context. The daemon atomically resolves an active matching session or creates one. Integrations do not need to persist a daemon token or short ID.

The working directory defines project identity. When a later resolution supplies a different label for the same working directory, the daemon updates the label in place without changing the project or session identity.

Each session has two identifiers.

- A short public ID appears in URLs and CLI output. The daemon allocates six characters from the Bitcoin Base58 alphabet (`123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz`) and increases the candidate length after a collision.
- An external key identifies the originating workflow within an integration namespace. It may be human-readable or opaque and does not need to appear in the viewer.

Agent-provided links open the session feed. The viewer also provides project and global feed scopes.

Explicitly closing a session immediately deletes its posts, managed snapshots, metadata, and unreferenced blobs. An inactive session is purged after seven days. New publications and a heartbeat from a visible session feed reset inactivity. Background listing or polling does not.

After purge, another publication with the same external key creates a fresh session. Glimse has no pin, archive, or saved-history mechanism.

## Posts and revisions

Agents publish posts explicitly. Glimse does not scan projects or watch directories.

A post contains:

- a title;
- Markdown commentary;
- one or more ordered visible files;
- an optional caption for each visible file;
- support assets used by Markdown or HTML documents;
- minimal project and Git provenance;
- an optional predecessor post for revisions.

Every post requires at least one visible artifact. Text-only status messages belong in the terminal.

Posts are immutable snapshots. Revising a result creates a new post at the top of the feed and links it to its predecessor. Publication is atomic, so a failed file, validation error, or storage limit leaves no partial post.

## Publication and storage

The CLI resolves source paths and streams file bytes to the daemon in a multipart request. The daemon never receives authority to read arbitrary source paths. A source path may be stored as inert provenance.

Glimse supports managed snapshots only. It does not serve referenced files from their original locations.

For Markdown and HTML entry documents, the CLI collects allowlisted relative resources beneath the entry directory. It preserves resource paths and rejects traversal, symlink escape, and unsupported dependency types. Supporting resources do not become visible feed items unless the publication lists them explicitly.

The daemon identifies blobs with SHA-256 encoded as 64 lowercase hexadecimal characters. Stored paths use two levels of two-character hash prefixes for bounded directory fan-out. Posts may share one stored blob, and purge removes a blob only after its final reference disappears. SQLite transactions coordinate metadata and blob-reference updates.

Configurable per-file and per-session byte limits reject a new publication before it becomes visible. Glimse never evicts older visible posts to make room.

The daemon determines media type through content sniffing and validates it against the filename and declared type. Dangerous mismatches fail publication. Safe explicit overrides may select a text language for highlighting.

## Feed behavior

The session feed is reverse chronological. Every visible artifact in a post renders inline in agent-specified order.

When a user is at the top, a new post appears immediately and shifts prior posts downward. When the user has scrolled away, the browser preserves the viewport and shows a new-content indicator. Activating the indicator returns to the new posts.

Complete inline rendering is intentional. Visually heavy posts encourage agents to publish focused figures and demonstrations. The browser may defer offscreen work, release media decoders, or virtualize text without changing the complete presentation.

The CLI always returns a deep link. It opens a local browser only when explicitly requested.

## Renderers

### Images and SVG

Images display at their natural dimensions up to the feed width and are not upscaled. A zoom-and-pan overlay exposes full resolution.

### PDF

PDF.js renders every page sequentially at feed width. Pages near the viewport materialize lazily to control browser memory without introducing nested document scrolling.

### Video and audio

Browser-native players display inline with controls. Media never autoplays. Players pause when they leave the viewport, and the server supports range requests.

### Markdown

Sanitized Markdown renders as a complete inline document. Relative resources resolve only to assets included in the published snapshot.

### Raw text and code

Plain text, logs, and highlighted source code render in resizable scroll panes with fullscreen support. Large files may virtualize lines but remain complete within configured byte limits.

### JSON and CSV

JSON uses a structured, resizable scroll pane. CSV uses a resizable, scrollable table. Both provide fullscreen inspection.

### HTML

HTML runs in a unique sandboxed iframe. Scripts may execute, but the document has no same-origin privilege and a restrictive content security policy blocks network access. All required resources must be included in the post snapshot.

### Unsupported files

Unsupported files remain downloadable from the post but do not receive a specialized renderer.

## Provenance

The CLI records the integration or agent name, external session key, project label, working directory, Git root, branch, and commit when available. It does not collect repository remotes, diffs, environment variables, or usernames by default.

## CLI and API contract

The CLI provides ergonomic flags for simple manual publication. Its canonical machine interface accepts a versioned JSON object on standard input containing session identity, commentary, ordered files, captions, and an optional predecessor. Commands support structured JSON output.

The daemon exposes a versioned local HTTP API. Native integrations may use the API directly, but the CLI remains the documented cross-agent interface.

## Distribution

Glimse targets Linux in its first release. Releases provide checksummed self-contained binaries. The CLI manages installation, startup, status, and removal of a systemd user service. Developers may also install through Cargo.

The same repository packages the generic agent skill and native pi extension.

## Deferred work

The first release does not include:

- browser-to-agent comments, annotations, or approvals;
- durable history, pins, or archives;
- automatic project scanning or watched directories;
- Office or notebook rendering;
- cloud publishing, accounts, or collaboration;
- macOS or Windows service integration.
