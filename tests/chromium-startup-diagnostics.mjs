import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";

const run = promisify(execFile);
const scripts = ["chromium-html-opt-in.mjs", "chromium-live-feed.mjs"];

test("Chromium startup failures report executable, elapsed time, exit and captured output", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "glim-fake-chromium-"));
  const executable = path.join(directory, "browser");
  try {
    await writeFile(executable, "#!/bin/sh\necho browser-stdout\necho browser-stderr >&2\nexit 37\n", { mode: 0o755 });
    for (const script of scripts) {
      await assert.rejects(run(process.execPath, [new URL(`../web/tests/${script}`, import.meta.url).pathname], {
        env: { ...process.env, CHROMIUM: executable }, timeout: 30_000,
      }), (error) => {
        assert.match(error.stderr, /Chromium debugging endpoint did not start/);
        assert.ok(error.stderr.includes(`executable=${executable}`), script);
        assert.match(error.stderr, /elapsed=\d+ms/);
        assert.match(error.stderr, /exit=code=37 signal=null/);
        assert.match(error.stderr, /spawn error=none/);
        assert.match(error.stderr, /Chromium stdout:\s*browser-stdout/);
        assert.match(error.stderr, /Chromium stderr:\s*browser-stderr/);
        return true;
      });
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("Chromium spawn errors report the OS error", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "glim-fake-chromium-"));
  const executable = path.join(directory, "browser");
  try {
    await writeFile(executable, "#!/bin/sh\n", { mode: 0o644 });
    await assert.rejects(run(process.execPath, [new URL("../web/tests/chromium-html-opt-in.mjs", import.meta.url).pathname], {
      env: { ...process.env, CHROMIUM: executable }, timeout: 30_000,
    }), (error) => {
      assert.match(error.stderr, /spawn error=.*EACCES/);
      assert.ok(error.stderr.includes(`executable=${executable}`));
      return true;
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
