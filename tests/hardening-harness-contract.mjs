import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { ACCEPTANCE_DEFAULTS, resolveAcceptanceConfiguration } from "./resource-acceptance-config.mjs";

const chromiumHarness = await readFile(new URL("../web/tests/chromium-live-feed.mjs", import.meta.url), "utf8");

test("default memory growth ceiling detects default full-file buffering", () => {
  assert.equal(ACCEPTANCE_DEFAULTS.fileBytes, 64 * 1024 * 1024);
  assert.equal(ACCEPTANCE_DEFAULTS.maxHwmGrowthBytes, 48 * 1024 * 1024);
  assert(ACCEPTANCE_DEFAULTS.maxHwmGrowthBytes < ACCEPTANCE_DEFAULTS.fileBytes);
  assert.deepEqual(resolveAcceptanceConfiguration({}), ACCEPTANCE_DEFAULTS);
});

test("acceptance overrides remain independently configurable", () => {
  const configuration = resolveAcceptanceConfiguration({
    GLIM_ACCEPT_FILE_BYTES: "1048576",
    GLIM_ACCEPT_MAX_HWM_GROWTH_BYTES: "2097152",
  });
  assert.equal(configuration.fileBytes, 1048576);
  assert.equal(configuration.maxHwmGrowthBytes, 2097152);
});

test("complete checks run Rust before Chromium while Chromium builds the frontend", async () => {
  const makefile = await readFile(new URL("../Makefile", import.meta.url), "utf8");
  const prerequisites = makefile.match(/^check: (.+)$/m)?.[1].split(" ");
  assert.ok(prerequisites, "make check must declare prerequisites");
  assert.ok(prerequisites.includes("rust-checks") && prerequisites.includes("chromium-security-check"), "complete checks must include Rust and Chromium");
  assert.ok(prerequisites.indexOf("rust-checks") < prerequisites.indexOf("chromium-security-check"), "Rust checks must precede Chromium checks");
  assert.match(makefile, /^chromium-security-check: build$/m);
  assert.match(makefile, /^build: frontend-build$/m);
});

test("ordinary Chromium daemon regression allocates an ephemeral port", () => {
  assert.doesNotMatch(chromiumHarness, /127\.0\.0\.1:3030|listen\(3030/);
  assert.match(chromiumHarness, /listen\(0, "127\.0\.0\.1"/);
  assert.match(chromiumHarness, /public_origin:\s*daemonOrigin/);
});
