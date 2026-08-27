import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);
const read = (path) => readFile(new URL(path, root), "utf8");

const harnessPath = new URL("tests/release-acceptance.mjs", root);

const requiredArtifacts = [
  "report.md",
  "support.png",
  "diagram.svg",
  "notes.txt",
  "data.json",
  "table.csv",
  "safe.html",
  "document.pdf",
  "tone.wav",
  "clip.mp4",
];

const requiredModels = [
  "openai-codex/gpt-5.6-sol",
  "openrouter/anthropic/claude-haiku-4.5",
];

const forbiddenCredentialPatterns = [
  /auth\.json/,
  /ANTHROPIC_API_KEY/,
  /OPENAI_API_KEY/,
  /OPENROUTER_API_KEY/,
  /--api-key/,
];

test("opt-in release acceptance is wired into Make and static CI only", async () => {
  await access(harnessPath);
  const makefile = await read("Makefile");
  assert.match(makefile, /^release-acceptance-check:\n\tnode --test tests\/release-acceptance-contract\.mjs$/m);
  assert.match(makefile, /^release-acceptance: release-acceptance-check\n\tnode tests\/release-acceptance\.mjs$/m);
  assert.match(makefile, /^check: .*release-acceptance-check/m);

  const ci = await read(".github/workflows/ci.yml");
  assert.match(ci, /run: make check/);
  assert.doesNotMatch(ci, /make release-acceptance(?:\s|$)/);
});

test("harness uses an installed candidate, isolated temporary state, and a local checked archive", async () => {
  const source = await read("tests/release-acceptance.mjs");
  assert.match(source, /cargo[\s\S]*install[\s\S]*--locked[\s\S]*--root/);
  assert.match(source, /GLIM_ACCEPT_CANDIDATE/);
  assert.match(source, /PATH:[\s\S]*cargoRoot[\s\S]*bin/);
  assert.match(source, /listen\(0,\s*["']127\.0\.0\.1["']/);
  assert.match(source, /access:[\s\S]*mode:[\s\S]*["']token["']/);
  assert.match(source, /env\.GLIM_DAEMON_URL\s*=\s*origin/, "candidate CLI must target the ephemeral acceptance daemon");
  assert.match(source, /sha256sum/);
  assert.match(source, /tar/);
  assert.doesNotMatch(source, /git\s+(?:tag|push)|gh\s+release/);
  assert.doesNotMatch(source, /["']service["']\s*,\s*["'](?:install|start|stop|uninstall)["']/);
  for (const pattern of forbiddenCredentialPatterns) assert.doesNotMatch(source, pattern);
});

test("fixtures and model matrix cover the release boundary without directory scans", async () => {
  const source = await read("tests/release-acceptance.mjs");
  for (const artifact of requiredArtifacts) assert.ok(source.includes(artifact), `missing fixture ${artifact}`);
  for (const model of requiredModels) assert.ok(source.includes(model), `missing model ${model}`);
  for (const mode of ["json", "rpc", "print"]) assert.match(source, new RegExp(`mode: ["']${mode}["']`));
  assert.match(source, /--no-extensions/);
  assert.match(source, /--no-skills/);
  assert.match(source, /--no-context-files/);
  assert.match(source, /--no-builtin-tools/);
  assert.match(source, /--tools["'],\s*["']glim_publish/);
  assert.match(source, /predecessor_post_id/);
  assert.match(source, /\/glim-feed/);
  assert.match(source, /\/glim-status/);
  assert.match(source, /\/glim-close/);
  assert.doesNotMatch(source, /readdir|glob\(|fast-glob|\/skill:glim/);
});

test("live checks are bounded, browser-backed, token-safe, isolated, and always cleaned", async () => {
  const source = await read("tests/release-acceptance.mjs");
  assert.match(source, /GLIM_ACCEPT_TIMEOUT_MS/);
  assert.match(source, /GLIM_ACCEPT_MODEL_TIMEOUT_MS/);
  assert.match(source, /GLIM_ACCEPT_MAX_OUTPUT_BYTES/);
  assert.match(source, /agent_settled/);
  assert.match(source, /tool_execution_end/);
  assert.match(source, /publicationMayHaveSucceeded|publication_may_have_succeeded/);
  assert.match(source, /cli_\$\{args\[0\]\}_exit/, "sanitized CLI blockers must identify the failed command boundary");
  assert.match(source, /--headless=new/);
  assert.match(source, /--user-data-dir/);
  assert.match(source, /Runtime\.exceptionThrown/);
  for (const renderer of ["image", "markdown", "text", "json", "csv", "html", "pdf", "audio", "video"]) {
    assert.ok(source.includes(`\"${renderer}\"`), `missing browser assertion for ${renderer}`);
  }
  assert.match(source, /for \(const renderer of rendererNames\) assert\(byRenderer\.has\(renderer\)/, "every renderer, including raster image, must be observed in Chromium");
  assert.doesNotMatch(source, /renderer === ["']image["'] \? ["']PASS/, "image acceptance must not be inferred from SVG coverage");
  assert.match(source, /assertLiveMatrixPassed\(\)/, "blocked live providers must fail release acceptance");
  assert.match(source, /extension_ui_request[\s\S]*method[\s\S]*notify/, "RPC commands must await their emitted notification");
  assert.doesNotMatch(source, /setTimeout\(resolve,\s*100\)/, "RPC command completion must not depend on a fixed sleep");
  assert.match(source, /active_sessions/);
  assert.match(source, /finalized_unique_blob_bytes/);
  assert.match(source, /queued_blob_deletions/);
  assert.match(source, /finally\s*{/);
  assert.match(source, /rm\([^)]*recursive:\s*true/);
  assert.doesNotMatch(source, /console\.(?:log|error)\([^\n]*(?:token|accessToken)/i);
});

test("public documentation reports the proven acceptance and preserves deferred release criteria", async () => {
  const readme = await read("README.md");
  assert.match(readme, /make release-acceptance/);
  assert.match(readme, /GLIM_RUN_LIVE_ACCEPTANCE=1/);
  assert.match(readme, /docs\/release-acceptance\.md/);

  const plan = await read("docs/implementation-plan.md");
  assert.match(plan, /clean-environment and live-model acceptance/i);
  assert.match(plan, /real release tag/i);
  assert.match(plan, /user-service install/i);

  const evidence = await read("docs/release-acceptance.md");
  for (const model of requiredModels) assert.ok(evidence.includes(model));
  for (const mode of ["JSON", "RPC", "print"]) assert.ok(evidence.includes(mode));
  assert.match(evidence, /real release tag/i);
  assert.match(evidence, /user-service install/i);
  assert.doesNotMatch(evidence, /https?:\/\/127\.0\.0\.1:\d+\/sessions\/[A-Za-z0-9]+/);
  assert.doesNotMatch(evidence, /\b[a-f0-9]{64}\b/i);
  assert.doesNotMatch(evidence, /\/tmp\//);
});
