import { chmod, mkdtemp, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, test } from "vitest";
import { runGlim } from "../src/client.js";

async function fake(body: string) {
  const dir = await mkdtemp(join(tmpdir(), "glim pi test "));
  const path = join(dir, "fake glim");
  await writeFile(path, `#!/usr/bin/env node\n${body}\n`);
  await chmod(path, 0o755);
  return path;
}

const ok = `process.stdin.setEncoding("utf8"); let input=""; process.stdin.on("data", c => input += c); process.stdin.on("end", () => process.stdout.write(JSON.stringify({schema_version:1,ok:true,result:{argv:process.argv.slice(2),input:JSON.parse(input)}})));`;

describe("CLI subprocess adapter", () => {
  test("uses exact argv without a shell and sends one bounded JSON document on closed stdin", async () => {
    const executable = await fake(ok);
    const input = { title: "quotes ' ; $(touch nope)", token_name: "not-a-secret" };
    const output: any = await runGlim(["publish", "--json", "--open"], input, undefined, { executable });
    expect(output.result).toEqual({ argv: ["publish", "--json", "--open"], input });
  });

  test.each([
    ["empty", "", 0, "empty_output"],
    ["multiple JSON", '{"schema_version":1,"ok":true,"result":{}} {"x":1}', 0, "malformed_output"],
    ["malformed JSON", "not json", 0, "malformed_output"],
    ["zero ok:false", '{"schema_version":1,"ok":false,"error":{"code":"denied","message":"no","details":{}}}', 0, "contract_contradiction"],
    ["nonzero ok:true", '{"schema_version":1,"ok":true,"result":{}}', 2, "contract_contradiction"],
  ])("rejects %s", async (_name, stdout, exit, code) => {
    const executable = await fake(`process.stdin.resume(); process.stdin.on("end",()=>{process.stdout.write(${JSON.stringify(stdout)}); process.exitCode=${exit}});`);
    await expect(runGlim(["status"], undefined, undefined, { executable })).rejects.toMatchObject({ code });
  });

  test("preserves stable confirmed CLI rejection and ambiguity details", async () => {
    const rejected = JSON.stringify({ schema_version: 1, ok: false, error: { code: "validation_error", message: "no", details: {} } });
    const rejectedExecutable = await fake(`process.stdin.resume(); process.stdin.on("end",()=>{process.stdout.write(${JSON.stringify(rejected)}); process.exitCode=2});`);
    await expect(runGlim(["publish", "--json"], { safe: true }, undefined, { executable: rejectedExecutable })).rejects.toMatchObject({
      code: "validation_error", exitCode: 2, publicationMayHaveSucceeded: false,
    });

    const ambiguous = JSON.stringify({ schema_version: 1, ok: false, error: { code: "daemon_unavailable", message: "connection lost", details: { publication_may_have_succeeded: true } } });
    const ambiguousExecutable = await fake(`process.stdin.resume(); process.stdin.on("end",()=>{process.stdout.write(${JSON.stringify(ambiguous)}); process.exitCode=3});`);
    await expect(runGlim(["publish", "--json"], { safe: true }, undefined, { executable: ambiguousExecutable })).rejects.toMatchObject({
      code: "daemon_unavailable", exitCode: 3, publicationMayHaveSucceeded: true,
    });
  });

  test("marks extension-level publication output and lifecycle failures as ambiguous", async () => {
    const malformed = await fake(`process.stdin.resume(); process.stdin.on("end",()=>process.stdout.write("not json"));`);
    await expect(runGlim(["publish", "--json"], { safe: true }, undefined, { executable: malformed })).rejects.toMatchObject({
      code: "malformed_output", publicationMayHaveSucceeded: true,
    });

    const hanging = await fake(`process.stdin.resume(); setTimeout(()=>{}, 10000);`);
    await expect(runGlim(["publish", "--json"], { safe: true }, undefined, { executable: hanging, timeoutMs: 30 })).rejects.toMatchObject({
      code: "timeout", publicationMayHaveSucceeded: true,
    });
    await expect(runGlim(["status"], undefined, undefined, { executable: hanging, timeoutMs: 30 })).rejects.toMatchObject({
      code: "timeout", publicationMayHaveSucceeded: false,
    });
  });

  test("handles spawn failure without exposing input or environment credentials", async () => {
    const manifest = { commentary: "MANIFEST-SENTINEL" };
    process.env.GLIM_ACCESS_TOKEN = "TOKEN-SENTINEL";
    try {
      await runGlim(["publish", "--json"], manifest, undefined, { executable: "/definitely/missing/glim" });
      throw new Error("expected failure");
    } catch (error: any) {
      expect(error.code).toBe("spawn_failed");
      expect(error.message).not.toMatch(/MANIFEST-SENTINEL|TOKEN-SENTINEL/);
    } finally {
      delete process.env.GLIM_ACCESS_TOKEN;
    }
  });

  test("kills on timeout", async () => {
    const executable = await fake(`process.stdin.resume(); setTimeout(()=>{}, 10000);`);
    await expect(runGlim(["status"], undefined, undefined, { executable, timeoutMs: 30 })).rejects.toMatchObject({ code: "timeout" });
  });

  test("kills on abort", async () => {
    const executable = await fake(`process.stdin.resume(); setTimeout(()=>{}, 10000);`);
    const controller = new AbortController();
    setTimeout(() => controller.abort(), 30);
    await expect(runGlim(["status"], undefined, controller.signal, { executable })).rejects.toMatchObject({ code: "cancelled" });
  });

  test("kills and rejects stdout or stderr overflow", async () => {
    const stdout = await fake(`process.stdin.resume(); process.stdout.write("x".repeat(5000));`);
    await expect(runGlim(["status"], undefined, undefined, { executable: stdout, maxOutputBytes: 1024 })).rejects.toMatchObject({ code: "output_too_large" });
    const stderr = await fake(`process.stdin.resume(); process.stderr.write("x".repeat(5000));`);
    await expect(runGlim(["status"], undefined, undefined, { executable: stderr, maxOutputBytes: 1024 })).rejects.toMatchObject({ code: "output_too_large" });
  });

  test("rejects an oversized input before spawning and never includes it in the error", async () => {
    const sentinel = "PRIVATE-MANIFEST";
    await expect(runGlim(["publish", "--json"], { text: sentinel.repeat(200) }, undefined, {
      executable: "/unused", maxInputBytes: 100,
    })).rejects.toSatisfy((error: any) => error.code === "input_too_large" && !error.message.includes(sentinel));
  });
});
