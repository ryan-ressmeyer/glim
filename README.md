# Glimse

Glimse is an ephemeral browser feed for visual output produced by terminal-based AI agents. Agents publish images, PDFs, video, audio, rendered documents, and structured text through the `glim` command. A local web service presents those results with the agent's commentary in a live, session-scoped feed.

The project is in its planning stage. No viewer or daemon functionality has been implemented yet.

## Design goals

- Work with any shell-capable agent through a standalone CLI.
- Provide native pi integration without coupling the core service to pi.
- Keep review sessions temporary and purge their managed data when they close.
- Render complete results in the browser rather than reducing them to terminal links or thumbnails.
- Bind to loopback safely by default while supporting authenticated remote access.
- Ship as a self-contained Linux binary with a managed systemd user service.

The agreed product design is recorded in [`docs/product-design.md`](docs/product-design.md). The dependency-ordered build plan is in [`docs/implementation-plan.md`](docs/implementation-plan.md).

## Current state

The repository contains a placeholder Rust binary so the project can establish its design, tests, and release structure before feature implementation begins.

## License

Glimse is available under the [MIT License](LICENSE).
