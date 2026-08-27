import { spawn } from "node:child_process";

import type { CliSuccess } from "./types.js";

const DEFAULT_MAX_INPUT_BYTES = 1024 * 1024;
const DEFAULT_MAX_OUTPUT_BYTES = 1024 * 1024 + 4096;
/** Large allowed artifact uploads are streamed by the CLI; ten minutes is finite but intentionally generous. */
export const DEFAULT_TIMEOUT_MS = 10 * 60 * 1000;
const MAX_ERROR_MESSAGE = 4096;
const MAX_DETAIL_STRING = 2048;

export interface RunOptions {
  executable?: string;
  timeoutMs?: number;
  maxInputBytes?: number;
  maxOutputBytes?: number;
}

export type CliRunner = (
  args: string[],
  input?: unknown,
  signal?: AbortSignal,
) => Promise<CliSuccess<any>>;

export class GlimCliError extends Error {
  code: string;
  details: Record<string, unknown>;
  exitCode?: number;
  publicationMayHaveSucceeded: boolean;

  constructor(code: string, message: string, options: {
    details?: Record<string, unknown>;
    exitCode?: number;
    publicationMayHaveSucceeded?: boolean;
  } = {}) {
    const safeCode = bounded(code, 128);
    const safeDetails = sanitizeDetails(options.details ?? {});
    const mayHaveSucceeded = options.publicationMayHaveSucceeded === true;
    super(bounded(JSON.stringify({
      code: safeCode,
      message: bounded(redact(message), 2048),
      details: safeDetails,
      ...(options.exitCode === undefined ? {} : { exit_code: options.exitCode }),
      publication_may_have_succeeded: mayHaveSucceeded,
    }), MAX_ERROR_MESSAGE));
    this.name = "GlimCliError";
    this.code = safeCode;
    this.details = safeDetails;
    this.exitCode = options.exitCode;
    this.publicationMayHaveSucceeded = mayHaveSucceeded;
  }
}

function bounded(value: unknown, limit: number): string {
  const text = String(value).replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, "�");
  return text.length <= limit ? text : `${text.slice(0, limit - 1)}…`;
}

function secretValues(): string[] {
  return Object.entries(process.env)
    .filter(([name, value]) => value && /(token|secret|password|api.?key|credential)/i.test(name))
    .map(([, value]) => value as string)
    .filter((value) => value.length >= 4);
}

function redact(value: string): string {
  let result = value;
  for (const secret of secretValues()) result = result.split(secret).join("[redacted]");
  return result.replace(/(https?:\/\/)[^\s/@:]+:[^\s/@]+@/gi, "$1[redacted]@");
}

function sanitizeDetails(value: Record<string, unknown>): Record<string, unknown> {
  let remaining = 16 * 1024;
  const visit = (item: unknown, depth: number): unknown => {
    if (remaining <= 0) return "[truncated]";
    if (typeof item === "string") {
      const clean = bounded(redact(item), Math.min(MAX_DETAIL_STRING, remaining));
      remaining -= clean.length;
      return clean;
    }
    if (typeof item === "number" || typeof item === "boolean" || item === null) return item;
    if (depth >= 4) return "[bounded]";
    if (Array.isArray(item)) return item.slice(0, 64).map((part) => visit(part, depth + 1));
    if (item && typeof item === "object") {
      const output: Record<string, unknown> = {};
      for (const [key, part] of Object.entries(item).slice(0, 64)) output[bounded(key, 128)] = visit(part, depth + 1);
      return output;
    }
    return bounded(item, 128);
  };
  return visit(value, 0) as Record<string, unknown>;
}

function parseEnvelope(stdout: Buffer, exitCode: number, stderr: Buffer, publication: boolean): CliSuccess<any> {
  const ambiguous = { exitCode, publicationMayHaveSucceeded: publication };
  const text = stdout.toString("utf8").trim();
  if (!text) throw new GlimCliError("empty_output", "glim returned no JSON", ambiguous);
  let envelope: any;
  try {
    envelope = JSON.parse(text);
  } catch {
    throw new GlimCliError("malformed_output", "glim returned malformed or multiple JSON values", ambiguous);
  }
  if (!envelope || typeof envelope !== "object" || envelope.schema_version !== 1 || typeof envelope.ok !== "boolean") {
    throw new GlimCliError("malformed_output", "glim returned an invalid standard envelope", ambiguous);
  }
  if (exitCode === 0 && envelope.ok === false) {
    throw new GlimCliError("contract_contradiction", "glim exited zero with ok:false", { exitCode });
  }
  if (exitCode !== 0 && envelope.ok === true) {
    throw new GlimCliError("contract_contradiction", "glim exited nonzero with ok:true", ambiguous);
  }
  if (envelope.ok === false) {
    const error = envelope.error;
    if (!error || typeof error.code !== "string" || typeof error.message !== "string" || !error.details || typeof error.details !== "object" || Array.isArray(error.details)) {
      throw new GlimCliError("malformed_output", "glim returned an invalid error envelope", { exitCode });
    }
    throw new GlimCliError(error.code, error.message, {
      details: error.details,
      exitCode,
      publicationMayHaveSucceeded: error.details.publication_may_have_succeeded === true,
    });
  }
  if (!("result" in envelope)) throw new GlimCliError("malformed_output", "glim success envelope is missing result", ambiguous);
  if (stderr.length > 0) {
    throw new GlimCliError("unexpected_stderr", `glim wrote unexpected stderr: ${bounded(redact(stderr.toString("utf8")), 1024)}`, ambiguous);
  }
  return envelope as CliSuccess<any>;
}

export async function runGlim(
  args: string[],
  input?: unknown,
  signal?: AbortSignal,
  options: RunOptions = {},
): Promise<CliSuccess<any>> {
  const executable = options.executable ?? "glim";
  const publication = args[0] === "publish";
  const maxInputBytes = options.maxInputBytes ?? DEFAULT_MAX_INPUT_BYTES;
  const maxOutputBytes = options.maxOutputBytes ?? DEFAULT_MAX_OUTPUT_BYTES;
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  let stdin: Buffer | undefined;
  if (input !== undefined) {
    try {
      stdin = Buffer.from(JSON.stringify(input), "utf8");
    } catch {
      throw new GlimCliError("input_encoding_failed", "publication input could not be serialized");
    }
    if (stdin.length > maxInputBytes) throw new GlimCliError("input_too_large", "publication JSON exceeds 1 MiB");
  }
  if (signal?.aborted) throw new GlimCliError("cancelled", "glim command was cancelled");

  return await new Promise((resolve, reject) => {
    const child = spawn(executable, args, { shell: false, stdio: ["pipe", "pipe", "pipe"] });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let terminalError: GlimCliError | undefined;
    let settled = false;
    let started = false;

    const lifecycleError = (code: string, message: string) => new GlimCliError(code, message, {
      publicationMayHaveSucceeded: publication && started,
    });
    const fail = (error: GlimCliError) => {
      if (!terminalError) terminalError = error;
      child.kill("SIGKILL");
    };
    const timeout = setTimeout(() => fail(lifecycleError("timeout", `glim command exceeded ${timeoutMs} ms`)), timeoutMs);
    const abort = () => fail(lifecycleError("cancelled", "glim command was cancelled"));
    signal?.addEventListener("abort", abort, { once: true });
    child.once("spawn", () => { started = true; });

    child.stdout.on("data", (chunk: Buffer) => {
      stdoutBytes += chunk.length;
      if (stdoutBytes > maxOutputBytes) return fail(lifecycleError("output_too_large", "glim stdout exceeded the response limit"));
      stdout.push(chunk);
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderrBytes += chunk.length;
      if (stderrBytes > maxOutputBytes) return fail(lifecycleError("output_too_large", "glim stderr exceeded the response limit"));
      stderr.push(chunk);
    });
    child.on("error", () => fail(started
      ? lifecycleError("process_error", "glim process failed after starting")
      : new GlimCliError("spawn_failed", "could not start glim from PATH")));
    child.stdin.on("error", () => fail(lifecycleError("stdin_error", "could not send bounded JSON to glim")));
    child.on("close", (code, childSignal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      signal?.removeEventListener("abort", abort);
      if (terminalError) return reject(terminalError);
      if (childSignal) return reject(lifecycleError("process_signal", `glim terminated by ${bounded(childSignal, 32)}`));
      try {
        resolve(parseEnvelope(Buffer.concat(stdout), code ?? -1, Buffer.concat(stderr), publication));
      } catch (error) {
        reject(error);
      }
    });
    if (stdin) child.stdin.end(stdin);
    else child.stdin.end();
  });
}
