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

The daemon binds to loopback by default. It reads a versioned JSON file from the user configuration directory, with environment variables taking precedence. Loopback ports are configurable. A non-loopback address fails startup until one of the authenticated modes below is selected.

Non-loopback deployments require one of two explicit modes.

- Built-in token authentication uses a generated persistent token. Direct non-loopback binding requires configured TLS. CLI clients send a Bearer credential; browsers exchange the token for a bounded HttpOnly session cookie. Sandboxed HTML uses short-lived capabilities restricted to its declared support assets.
- Trusted-proxy mode delegates TLS and user authentication to a user-configured proxy or private network boundary. Glimse accepts requests only from configured exact numeric TCP peer addresses and does not trust forwarded identity headers. Health remains available for proxy probes. Other routes require an allowlisted peer, browser-originated mutations require the configured exact public origin, and headerless non-browser clients rely on peer authorization. Token login is unavailable in this mode.

The default configuration must not expose artifacts to the local network. A trusted-proxy listener must also be unreachable by non-proxy clients because possession of an allowlisted source address is the daemon's complete authorization check. Non-loopback trusted-proxy deployments require an HTTPS public origin while the proxy-to-daemon connection may use HTTP. Media endpoints support HTTP range requests for seeking and partial transfer.

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

A configurable global physical blob budget counts each unique finalized blob once. Deduplicated uploads consume no additional finalized-store budget. A separate configurable per-file upload ceiling bounds each staging write and guards against abusive uploads. Temporary staging may consume bounded transient disk space outside finalized-store accounting, and ordinary filesystem-full errors remain possible.

The publication API will reject a whole publication before it becomes visible when any file exceeds the upload ceiling or its new unique blobs would exceed the global budget. Glimse never evicts older visible posts to make room. Production default byte values remain undecided.

The daemon determines media type through content sniffing and validates it against the filename and declared type. Dangerous mismatches fail publication. Safe explicit overrides may select a text language for highlighting.

## Feed behavior

The session feed is reverse chronological. Every visible artifact in a post renders inline in agent-specified order.

When a user is at the top, a new post appears immediately and shifts prior posts downward. When the user has scrolled away, the browser preserves the viewport and shows a new-content indicator. Activating the indicator returns to the new posts. The browser retains at most 100 pending posts; a larger burst triggers latest-page reconciliation.

Scoped server-sent event streams use post IDs as event IDs. The daemon retains 256 live events and replays at most 100 durable posts after reconnection. A client that exceeds either bound receives a reset event and reconciles from the latest page. Session closure events stop matching session views and prompt project or global views to reconcile removed posts.

A visible session page sends a heartbeat every 30 seconds only while its event stream is open. Hiding the page, losing the stream, closing the session, or disconnecting the component stops heartbeat work. Session pages require native destructive confirmation before closing.

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

HTML renders inline in a unique sandboxed iframe with scripts disabled by default. The renderer parses the entry document without attaching it, removes untrusted document policies, nested browsing and plugin elements, disables forms and external navigation, and rewrites declared resources to the exact support-asset routes for that file. A deterministic content security policy blocks connections, forms, frames, objects, undeclared subresources, and scripts in the default mode.

Each artifact includes a warning control that reloads the iframe with only `allow-scripts` added to the sandbox. Script mode permits inline scripts and scripts from that artifact's exact support path. It does not add same-origin access or permit forms, popups, downloads, modals, or top-level navigation. The content security policy keeps `connect-src 'none'` and continues to block undeclared subresources. A script can still navigate its own frame and thereby make a network request because browsers do not provide a sandbox token that blocks self-navigation while allowing scripts. The warning discloses this limitation, and script mode is never automatic.

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
