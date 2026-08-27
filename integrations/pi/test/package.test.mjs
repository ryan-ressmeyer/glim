import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";
import { test } from "node:test";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "../../..");
const manifest = JSON.parse(await readFile(resolve(root, "package.json"), "utf8"));

test("root manifest discovers exactly the Pi extension and generic skill", async () => {
  assert.deepEqual(manifest.keywords, ["pi-package"]);
  assert.deepEqual(manifest.pi, {
    extensions: ["./integrations/pi/src/index.ts"],
    skills: ["./integrations/generic-skill/glim"],
  });
  await access(resolve(root, "integrations/pi/src/index.ts"));
  await access(resolve(root, "integrations/generic-skill/glim/SKILL.md"));
});

test("Pi imports are peers and test-only packages are development dependencies", async () => {
  assert.deepEqual(manifest.peerDependencies, {
    "@earendil-works/pi-coding-agent": "*",
    typebox: "*",
  });
  assert.equal(manifest.dependencies, undefined);
  for (const name of ["typescript", "vitest", "@types/node"]) {
    assert.ok(manifest.devDependencies[name]);
  }
  await access(resolve(root, "package-lock.json"));
});

test("Pi tests exclude generated Cargo package trees", () => {
  assert.match(manifest.scripts.test, /--exclude ['"]?target\/\*\*['"]?/);
});

test("extension delegates HTTP, authentication, assets, and Git handling to the CLI", async () => {
  const source = await readFile(resolve(root, "integrations/pi/src/index.ts"), "utf8");
  assert.doesNotMatch(source, /from ["']node:https?["']/);
  assert.doesNotMatch(source, /\bfetch\s*\(/);
  assert.doesNotMatch(source, /GLIM_(?:TOKEN|DAEMON_URL|STORE_ROOT)|Authorization|Bearer/);
  assert.doesNotMatch(source, /collect_support_assets|git_output|rev-parse/);
});
