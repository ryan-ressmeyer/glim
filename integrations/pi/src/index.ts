import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { basename, resolve } from "node:path";
import { Type, type Static } from "typebox";

import { GlimCliError, runGlim, type CliRunner } from "./client.js";
import type { BrowserLaunch, PublicationDetails } from "./types.js";

export const GLIM_CLOSE_ENTRY = "glimse-pi-close-v1";
const COMMAND_MESSAGE = "glimse-pi-command-v1";
const MAX_COMMAND_TEXT = 4096;

const FileSchema = Type.Object({
  path: Type.String({ minLength: 1, maxLength: 2048, description: "Artifact path, resolved relative to the workspace" }),
  published_filename: Type.Optional(Type.String({ minLength: 1, maxLength: 255 })),
  caption: Type.Optional(Type.String({ maxLength: 512 })),
  media_type: Type.Optional(Type.String({ minLength: 1, maxLength: 255 })),
  collect_assets: Type.Optional(Type.Boolean({ default: true })),
}, { additionalProperties: false });

export const GlimPublishSchema = Type.Object({
  title: Type.String({ minLength: 1, maxLength: 4096, description: "Concise nonblank publication title" }),
  commentary: Type.String({ minLength: 1, maxLength: 65536, description: "Nonblank Markdown explaining what to inspect" }),
  files: Type.Array(FileSchema, { minItems: 1, maxItems: 256, description: "Ordered artifacts" }),
  predecessor_post_id: Type.Optional(Type.Integer({ minimum: 1, maximum: Number.MAX_SAFE_INTEGER, description: "Earlier immutable post revised by this publication" })),
  open: Type.Optional(Type.Boolean({ default: false, description: "Request browser launch after publication" })),
}, { additionalProperties: false });

export type GlimPublishInput = Static<typeof GlimPublishSchema>;

interface PublicationResult {
  session: { public_id: string };
  post: { id: number };
  viewer_url: string;
  post_url: string;
  browser_launch: BrowserLaunch;
}

interface Dependencies {
  run: CliRunner;
}

function bounded(text: unknown, limit = MAX_COMMAND_TEXT): string {
  const value = String(text).replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, "�");
  return value.length <= limit ? value : `${value.slice(0, limit - 1)}…`;
}

function requirePublicationResult(value: unknown): PublicationResult {
  const result = value as any;
  if (!result || typeof result !== "object" || typeof result.session?.public_id !== "string" || !result.session.public_id ||
      !Number.isInteger(result.post?.id) || result.post.id <= 0 || typeof result.viewer_url !== "string" || !result.viewer_url ||
      typeof result.post_url !== "string" || !result.post_url || !result.browser_launch || typeof result.browser_launch.requested !== "boolean" ||
      typeof result.browser_launch.opened !== "boolean") {
    throw new GlimCliError("malformed_cli_result", "validated CLI publication result is missing required fields", {
      publicationMayHaveSucceeded: true,
    });
  }
  return result as PublicationResult;
}

function isPublicationDetails(value: unknown, piSessionId: string): value is PublicationDetails {
  const item = value as any;
  return !!item && typeof item === "object" && item.pi_session_id === piSessionId &&
    typeof item.public_session_id === "string" && item.public_session_id.length > 0 &&
    Number.isInteger(item.post_id) && item.post_id > 0 && typeof item.viewer_url === "string" && item.viewer_url.length > 0 &&
    typeof item.post_url === "string" && item.post_url.length > 0 && typeof item.external_session_key === "string" &&
    typeof item.project_label === "string" && typeof item.working_directory === "string" && item.browser_launch && typeof item.browser_launch === "object";
}

function externalKey(sessionId: string): string {
  return `pi-${sessionId}`;
}

function projectLabel(cwd: string): string {
  return basename(resolve(cwd)) || "workspace";
}

function normalizePath(cwd: string, path: string): string {
  const normalized = path.startsWith("@") ? path.slice(1) : path;
  return resolve(cwd, normalized);
}

function normalizeFailure(error: unknown): GlimCliError {
  if (error instanceof GlimCliError) return error;
  if (error && typeof error === "object") {
    const item = error as any;
    const details = item.details && typeof item.details === "object" ? item.details : {};
    return new GlimCliError(typeof item.code === "string" ? item.code : "cli_error", item.message ?? "glim command failed", {
      details,
      exitCode: typeof item.exitCode === "number" ? item.exitCode : undefined,
      publicationMayHaveSucceeded: item.publicationMayHaveSucceeded === true || details.publication_may_have_succeeded === true,
    });
  }
  return new GlimCliError("cli_error", "glim command failed");
}

function formatFailure(error: unknown): string {
  if (error instanceof GlimCliError) {
    const ambiguity = error.publicationMayHaveSucceeded ? " Publication may have succeeded; inspect state before retrying." : "";
    return bounded(`Glimse error ${error.code}${error.exitCode === undefined ? "" : ` (exit ${error.exitCode})`}: ${error.message}${ambiguity}`);
  }
  if (error && typeof error === "object" && "code" in error) {
    return bounded(`Glimse error ${String((error as any).code)}: ${String((error as any).message ?? "command failed")}`);
  }
  return "Glimse command failed.";
}

export function createGlimExtension(dependencies: Dependencies) {
  return function glimExtension(pi: ExtensionAPI): void {
    let current: PublicationDetails | undefined;

    const reconstruct = (ctx: ExtensionContext) => {
      current = undefined;
      const sessionId = ctx.sessionManager.getSessionId();
      for (const entry of ctx.sessionManager.getBranch() as any[]) {
        if (entry.type === "message" && entry.message?.role === "toolResult" && entry.message.toolName === "glim_publish" &&
            entry.message.isError !== true && isPublicationDetails(entry.message.details, sessionId)) {
          current = entry.message.details;
        } else if (entry.type === "custom" && entry.customType === GLIM_CLOSE_ENTRY && entry.data?.pi_session_id === sessionId &&
                   current && entry.data.public_session_id === current.public_session_id) {
          current = undefined;
        }
      }
    };

    const emit = (ctx: ExtensionContext, text: string, level: "info" | "warning" | "error" = "info") => {
      const safe = bounded(text);
      if (ctx.hasUI) {
        ctx.ui.notify(safe, level);
      } else {
        pi.sendMessage({ customType: COMMAND_MESSAGE, content: safe, display: true, details: { level } }, { deliverAs: "nextTurn" });
      }
    };

    pi.on("session_start", async (_event, ctx) => reconstruct(ctx));
    pi.on("session_tree", async (_event, ctx) => reconstruct(ctx));

    pi.registerTool({
      name: "glim_publish",
      label: "Publish with Glimse",
      description: "Publish one immutable Glimse post containing an ordered set of deliberate visual or inspectable artifacts. The existing glim CLI handles files, assets, Git provenance, authentication, HTTP, and daemon validation.",
      promptSnippet: "Publish deliberate inspectable artifacts as an immutable Glimse post",
      promptGuidelines: [
        "Use glim_publish for deliberate visual or inspectable artifacts that benefit from browser inspection.",
        "Do not use glim_publish for routine diffs, tests, logs, terminal transcripts, or prose-only status updates.",
      ],
      parameters: GlimPublishSchema,
      async execute(_toolCallId, params, signal, _onUpdate, ctx) {
        if (!params.title.trim()) throw new GlimCliError("validation_error", "title must not be blank");
        if (!params.commentary.trim()) throw new GlimCliError("validation_error", "commentary must not be blank");
        const piSessionId = ctx.sessionManager.getSessionId();
        const cwd = resolve(ctx.cwd);
        const manifest: Record<string, unknown> = {
          schema_version: 1,
          integration_namespace: "pi",
          external_session_key: externalKey(piSessionId),
          project_label: projectLabel(cwd),
          working_directory: cwd,
          title: params.title,
          commentary: params.commentary,
          ...(params.predecessor_post_id === undefined ? {} : { predecessor_post_id: params.predecessor_post_id }),
          files: params.files.map((file) => ({
            source_path: normalizePath(cwd, file.path),
            ...(file.published_filename === undefined ? {} : { published_filename: file.published_filename }),
            ...(file.caption === undefined ? {} : { caption: file.caption }),
            ...(file.media_type === undefined ? {} : { media_type: file.media_type }),
            ...(file.collect_assets === undefined ? {} : { collect_assets: file.collect_assets }),
          })),
        };
        let envelope;
        try {
          envelope = await dependencies.run(["publish", "--json", ...(params.open ? ["--open"] : [])], manifest, signal);
        } catch (error) {
          throw normalizeFailure(error);
        }
        const result = requirePublicationResult(envelope.result);
        const details: PublicationDetails = {
          pi_session_id: piSessionId,
          external_session_key: externalKey(piSessionId),
          public_session_id: result.session.public_id,
          post_id: result.post.id,
          ...(params.predecessor_post_id === undefined ? {} : { predecessor_post_id: params.predecessor_post_id }),
          viewer_url: result.viewer_url,
          post_url: result.post_url,
          browser_launch: result.browser_launch,
          project_label: projectLabel(cwd),
          working_directory: cwd,
        };
        current = details;
        const browser = result.browser_launch.requested && !result.browser_launch.opened
          ? ` Browser launch failed (${bounded(result.browser_launch.error?.code ?? "unknown", 128)}); the publication remains confirmed.`
          : "";
        return {
          content: [{ type: "text" as const, text: `Glimse post ${result.post.id} published successfully: ${result.post_url}\nFeed: ${result.viewer_url}.${browser}` }],
          details,
        };
      },
    });

    pi.registerCommand("glim-feed", {
      description: "Show the confirmed Glimse feed for the current branch; add 'open' to launch it",
      handler: async (args, ctx) => {
        if (!current || current.pi_session_id !== ctx.sessionManager.getSessionId()) {
          emit(ctx, "No confirmed open Glimse publication exists on the current branch/session.", "info");
          return;
        }
        if (args.trim() === "open") {
          try {
            await dependencies.run(["open", current.viewer_url], undefined, undefined);
            emit(ctx, `Opened Glimse feed ${current.public_session_id}: ${current.viewer_url}`);
          } catch (error) {
            emit(ctx, formatFailure(error), "error");
          }
          return;
        }
        emit(ctx, `Glimse feed ${current.public_session_id}: ${current.viewer_url}`);
      },
    });

    pi.registerCommand("glim-status", {
      description: "Report bounded aggregate Glimse daemon health, storage, sessions, and cleanup status",
      handler: async (_args, ctx) => {
        try {
          const envelope = await dependencies.run(["status"], undefined, undefined);
          const status = envelope.result as any;
          const fields = ["ok", "version", "finalized_unique_blob_bytes", "max_upload_bytes", "max_finalized_blob_bytes", "active_sessions", "sessions_due_for_purge", "queued_blob_deletions", "retention_seconds", "cleanup_interval_seconds"];
          if (!status || typeof status !== "object" || fields.some((field) => !(field in status))) {
            throw new GlimCliError("malformed_cli_result", "status result is missing aggregate fields");
          }
          emit(ctx, bounded([
            `Glimse ${status.ok ? "healthy" : "unhealthy"} (version ${status.version})`,
            `storage: ${status.finalized_unique_blob_bytes}/${status.max_finalized_blob_bytes} bytes; upload limit: ${status.max_upload_bytes} bytes`,
            `active sessions: ${status.active_sessions}; due for purge: ${status.sessions_due_for_purge}; queued blob deletions: ${status.queued_blob_deletions}`,
            `retention: ${status.retention_seconds}s; cleanup interval: ${status.cleanup_interval_seconds}s`,
          ].join("\n")), status.ok ? "info" : "warning");
        } catch (error) {
          emit(ctx, formatFailure(error), "error");
        }
      },
    });

    pi.registerCommand("glim-close", {
      description: "Explicitly close and purge only the current branch's confirmed Glimse session",
      handler: async (_args, ctx) => {
        if (!current || current.pi_session_id !== ctx.sessionManager.getSessionId()) {
          emit(ctx, "No confirmed open Glimse publication exists on the current branch/session.", "info");
          return;
        }
        const closing = current;
        try {
          await dependencies.run(["close", closing.public_session_id], undefined, undefined);
          pi.appendEntry(GLIM_CLOSE_ENTRY, { pi_session_id: closing.pi_session_id, public_session_id: closing.public_session_id });
          current = undefined;
          emit(ctx, `Closed Glimse session ${closing.public_session_id}; its ephemeral feed and snapshots were purged.`);
        } catch (error) {
          emit(ctx, `${formatFailure(error)} Current Glimse state was preserved.`, "error");
        }
      },
    });
  };
}

export default createGlimExtension({ run: runGlim });
