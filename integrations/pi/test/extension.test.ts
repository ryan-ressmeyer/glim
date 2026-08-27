import { describe, expect, test, vi } from "vitest";
import { createGlimExtension, GLIM_CLOSE_ENTRY } from "../src/index.js";
import type { CliRunner } from "../src/client.js";

function success(publicId = "Public123", postId = 41, opened = false) {
  return {
    schema_version: 1 as const,
    ok: true as const,
    result: {
      session: { public_id: publicId, project_id: 7, external_key: "ignored" },
      post: { id: postId, session_public_id: publicId, title: "Plot" },
      viewer_url: `http://127.0.0.1:3030/sessions/${publicId}`,
      post_url: `http://127.0.0.1:3030/sessions/${publicId}#post-${postId}`,
      browser_launch: { requested: opened, opened: false, error: opened ? { code: "browser_launch_failed", message: "no display", details: {} } : null },
    },
  };
}

function harness(sessionId = "session-A", branch: unknown[] = [], hasUI = true) {
  const tools: any[] = [];
  const commands = new Map<string, any>();
  const handlers = new Map<string, any[]>();
  const entries: any[] = [];
  const messages: any[] = [];
  const notifications: any[] = [];
  const pi: any = {
    registerTool: (tool: any) => tools.push(tool),
    registerCommand: (name: string, command: any) => commands.set(name, command),
    on: (event: string, handler: any) => handlers.set(event, [...(handlers.get(event) ?? []), handler]),
    appendEntry: (customType: string, data: unknown) => entries.push({ customType, data }),
    sendMessage: (message: unknown, options: unknown) => messages.push({ message, options }),
  };
  const ctx: any = {
    cwd: "/work/my-project",
    mode: hasUI ? "tui" : "json",
    hasUI,
    sessionManager: { getSessionId: () => sessionId, getBranch: () => branch },
    ui: { notify: (text: string, level: string) => notifications.push({ text, level }) },
  };
  return { pi, ctx, tools, commands, handlers, entries, messages, notifications };
}

describe("registration", () => {
  test("registers one strict bounded publication tool and three distinct commands", () => {
    const h = harness();
    createGlimExtension({ run: vi.fn() })(h.pi);
    expect(h.tools).toHaveLength(1);
    const tool = h.tools[0];
    expect(tool.name).toBe("glim_publish");
    expect(tool.label).toBe("Publish with Glimse");
    expect(tool.description).toContain("immutable");
    expect(tool.promptSnippet).toContain("inspectable artifacts");
    expect(tool.promptGuidelines).toEqual(expect.arrayContaining([
      expect.stringContaining("glim_publish"),
      expect.stringContaining("routine diffs"),
    ]));
    expect(tool.parameters.additionalProperties).toBe(false);
    expect(tool.parameters.required).toEqual(["title", "commentary", "files"]);
    expect(tool.parameters.properties.open.default).toBe(false);
    expect(tool.parameters.properties.predecessor_post_id).toMatchObject({
      type: "integer", minimum: 1, maximum: Number.MAX_SAFE_INTEGER,
    });
    const files = tool.parameters.properties.files;
    expect(files).toMatchObject({ type: "array", minItems: 1, maxItems: 256 });
    expect(files.items.additionalProperties).toBe(false);
    expect(files.items.required).toEqual(["path"]);
    expect(Object.keys(files.items.properties)).toEqual(["path", "published_filename", "caption", "media_type", "collect_assets"]);
    expect([...h.commands.keys()]).toEqual(["glim-feed", "glim-status", "glim-close"]);
    expect(h.handlers.has("session_shutdown")).toBe(false);
  });
});

describe("publication", () => {
  test("maps exact canonical input, stable identity, paths, options, revision, and open argv", async () => {
    const run = vi.fn<CliRunner>().mockResolvedValue(success("Current99", 55, true));
    const h = harness("abc-123");
    createGlimExtension({ run })(h.pi);
    const result = await h.tools[0].execute("call", {
      title: " Result ", commentary: "Inspect both panels.", predecessor_post_id: 12, open: true,
      files: [
        { path: "@figures/a.png", caption: "A", collect_assets: false },
        { path: "/tmp/b.svg", published_filename: "second.svg", media_type: "image/svg+xml" },
      ],
    }, undefined, undefined, h.ctx);
    expect(run).toHaveBeenCalledTimes(1);
    expect(run).toHaveBeenCalledWith(["publish", "--json", "--open"], {
      schema_version: 1,
      integration_namespace: "pi",
      external_session_key: "pi-abc-123",
      project_label: "my-project",
      working_directory: "/work/my-project",
      title: " Result ",
      commentary: "Inspect both panels.",
      predecessor_post_id: 12,
      files: [
        { source_path: "/work/my-project/figures/a.png", caption: "A", collect_assets: false },
        { source_path: "/tmp/b.svg", published_filename: "second.svg", media_type: "image/svg+xml" },
      ],
    }, undefined);
    expect(result.details).toMatchObject({
      pi_session_id: "abc-123", external_session_key: "pi-abc-123", public_session_id: "Current99",
      post_id: 55, predecessor_post_id: 12, viewer_url: "http://127.0.0.1:3030/sessions/Current99",
      browser_launch: { requested: true, opened: false }, project_label: "my-project", working_directory: "/work/my-project",
    });
    expect(result.content[0].text).toContain("published successfully");
    expect(result.content[0].text).toContain("Browser launch failed");
  });

  test("reuses one Pi identity but isolates a new session", async () => {
    const run = vi.fn<CliRunner>().mockResolvedValue(success());
    const h = harness("one");
    createGlimExtension({ run })(h.pi);
    const args = { title: "T", commentary: "C", files: [{ path: "x.png" }] };
    await h.tools[0].execute("1", args, undefined, undefined, h.ctx);
    await h.tools[0].execute("2", args, undefined, undefined, h.ctx);
    const other = { ...h.ctx, sessionManager: { ...h.ctx.sessionManager, getSessionId: () => "two" } };
    await h.tools[0].execute("3", args, undefined, undefined, other);
    expect(run.mock.calls.map((call) => (call[1] as any).external_session_key)).toEqual(["pi-one", "pi-one", "pi-two"]);
  });

  test("throws one bounded stable error without retry", async () => {
    const error: any = new Error("daemon_rejected: denied");
    error.code = "daemon_rejected";
    error.details = { publication_may_have_succeeded: true, reason: "x".repeat(9000) };
    error.exitCode = 2;
    const run = vi.fn<CliRunner>().mockRejectedValue(error);
    const h = harness();
    createGlimExtension({ run })(h.pi);
    await expect(h.tools[0].execute("1", { title: "T", commentary: "C", files: [{ path: "x" }] }, undefined, undefined, h.ctx))
      .rejects.toMatchObject({ code: "daemon_rejected", exitCode: 2, publicationMayHaveSucceeded: true });
    expect(run).toHaveBeenCalledTimes(1);
    try { await h.tools[0].execute("2", { title: "T", commentary: "C", files: [{ path: "x" }] }, undefined, undefined, h.ctx); } catch (caught: any) {
      expect(caught.message.length).toBeLessThan(5000);
      expect(caught.message).toContain('"exit_code":2');
      expect(caught.message).toContain('"publication_may_have_succeeded":true');
      expect(caught.message).toContain('"reason"');
      expect(caught.message).not.toContain("x".repeat(5000));
    }
  });
});

describe("branch state and commands", () => {
  test("reconstructs successful local state, ignores malformed/foreign state, and honors close on tree updates", async () => {
    const valid = { pi_session_id: "session-A", public_session_id: "Here22", post_id: 8, viewer_url: "https://host/sessions/Here22", post_url: "https://host/sessions/Here22#post-8", browser_launch: { requested: false, opened: false, error: null }, external_session_key: "pi-session-A", project_label: "p", working_directory: "/p" };
    const branch: any[] = [
      { type: "message", message: { role: "toolResult", toolName: "glim_publish", isError: false, details: { ...valid, pi_session_id: "foreign" } } },
      { type: "message", message: { role: "toolResult", toolName: "glim_publish", isError: false, details: valid } },
    ];
    const h = harness("session-A", branch);
    createGlimExtension({ run: vi.fn() })(h.pi);
    await h.handlers.get("session_start")![0]({}, h.ctx);
    await h.commands.get("glim-feed").handler("", h.ctx);
    expect(h.notifications.at(-1).text).toContain("https://host/sessions/Here22");
    branch.push({ type: "custom", customType: GLIM_CLOSE_ENTRY, data: { pi_session_id: "session-A", public_session_id: "Here22" } });
    await h.handlers.get("session_tree")![0]({}, h.ctx);
    await h.commands.get("glim-feed").handler("", h.ctx);
    expect(h.notifications.at(-1).text).toContain("No confirmed open");
  });

  test("feed never creates a session, opens only the returned URL, and non-UI output is a non-triggering message", async () => {
    const valid: any = { pi_session_id: "session-A", public_session_id: "Exact77", post_id: 2, viewer_url: "https://elsewhere/exact", post_url: "https://elsewhere/exact#post-2", browser_launch: {}, external_session_key: "pi-session-A", project_label: "p", working_directory: "/p" };
    const branch = [{ type: "message", message: { role: "toolResult", toolName: "glim_publish", isError: false, details: valid } }];
    const run = vi.fn<CliRunner>().mockResolvedValue({ schema_version: 1, ok: true, result: { viewer_url: "https://elsewhere/exact" } });
    const h = harness("session-A", branch, false);
    createGlimExtension({ run })(h.pi);
    await h.handlers.get("session_start")![0]({}, h.ctx);
    await h.commands.get("glim-feed").handler("open", h.ctx);
    expect(run).toHaveBeenCalledWith(["open", "https://elsewhere/exact"], undefined, undefined);
    expect(h.messages.at(-1).options).toEqual({ deliverAs: "nextTurn" });
  });

  test("status displays only bounded aggregate fields", async () => {
    const run = vi.fn<CliRunner>().mockResolvedValue({ schema_version: 1, ok: true, result: {
      ok: true, version: "0.1.0", finalized_unique_blob_bytes: 10, max_upload_bytes: 20,
      max_finalized_blob_bytes: 30, active_sessions: 2, sessions_due_for_purge: 1,
      queued_blob_deletions: 3, retention_seconds: 604800, cleanup_interval_seconds: 3600,
      token: "secret", store_root: "/private/path",
    } });
    const h = harness();
    createGlimExtension({ run })(h.pi);
    await h.commands.get("glim-status").handler("", h.ctx);
    expect(h.notifications.at(-1).text).toContain("active sessions: 2");
    expect(h.notifications.at(-1).text).not.toMatch(/secret|private/);
  });

  test("close persists and clears only after confirmed success; failure preserves state", async () => {
    const valid: any = { pi_session_id: "session-A", public_session_id: "Close88", post_id: 2, viewer_url: "https://h/sessions/Close88", post_url: "https://h/sessions/Close88#post-2", browser_launch: {}, external_session_key: "pi-session-A", project_label: "p", working_directory: "/p" };
    const branch = [{ type: "message", message: { role: "toolResult", toolName: "glim_publish", isError: false, details: valid } }];
    const run = vi.fn<CliRunner>().mockRejectedValueOnce(Object.assign(new Error("busy"), { code: "daemon_unavailable" })).mockResolvedValueOnce({ schema_version: 1, ok: true, result: {} });
    const h = harness("session-A", branch);
    createGlimExtension({ run })(h.pi);
    await h.handlers.get("session_start")![0]({}, h.ctx);
    await h.commands.get("glim-close").handler("", h.ctx);
    await h.commands.get("glim-feed").handler("", h.ctx);
    expect(h.notifications.at(-1).text).toContain("Close88");
    await h.commands.get("glim-close").handler("", h.ctx);
    expect(run.mock.calls[1][0]).toEqual(["close", "Close88"]);
    expect(h.entries).toEqual([{ customType: GLIM_CLOSE_ENTRY, data: { pi_session_id: "session-A", public_session_id: "Close88" } }]);
    expect(h.notifications.at(-1).text).toContain("purged");
  });
});
