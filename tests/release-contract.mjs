import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import test from "node:test";

const root = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, root), "utf8");

function cargoMetadata() {
  return JSON.parse(execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps", "--locked"], {
    cwd: root,
    encoding: "utf8",
  })).packages.find((pkg) => pkg.name === "glim");
}

test("crate metadata and build inputs support installation from a source checkout", () => {
  const pkg = cargoMetadata();
  assert.equal(pkg.license, "MIT");
  assert.equal(pkg.readme, "README.md");
  assert.equal(pkg.repository, "https://github.com/ryan-ressmeyer/glim");
  assert.match(pkg.description, /visual output/i);
  assert.deepEqual(pkg.keywords, ["ai-agents", "artifacts", "cli", "visualization"]);
  assert.deepEqual(pkg.categories, ["command-line-utilities", "development-tools"]);

  const manifest = read("Cargo.toml");
  assert.match(manifest, /^build = "build\.rs"$/m);
  const build = read("build.rs");
  assert.match(build, /npm[\s\S]*\bci\b/);
  assert.match(build, /npm[\s\S]*run[\s\S]*build/);
  for (const asset of ["index.html", "assets/app.js", "assets/pdf.worker.mjs"]) {
    assert.ok(build.includes(asset), `build script must stage ${asset}`);
  }
});

test("frontend build ignores stale dist and confines generated state to OUT_DIR", () => {
  const build = read("build.rs");
  assert.doesNotMatch(
    build,
    /(?:Path::new\("web\/dist"\)|\bweb\.join\("dist"\))/,
    "build script must never consume source-tree web/dist",
  );
  assert.match(build, /env::var\("OUT_DIR"\)[\s\S]*join\("frontend-build"\)/);
  assert.match(build, /remove_dir_all\(&build_workspace\)/);
  const npmInvocations = [...build.matchAll(/^\s+run_npm\(([^;]+)\);$/gm)].map((match) => match[1]);
  assert.deepEqual(npmInvocations, [
    "&build_workspace, &[\"ci\"]",
    "&build_workspace, &[\"run\", \"build\"]",
  ]);
  assert.match(build, /build_workspace\.join\("dist"\)/);
  assert.ok(build.includes("cargo:rerun-if-changed={input}"));
  for (const input of [
    "web/index.html",
    "web/package.json",
    "web/package-lock.json",
    "web/tsconfig.json",
    "web/vite.config.ts",
    "web/src",
  ]) {
    assert.ok(build.includes(`"${input}"`), `missing rerun input for ${input}`);
  }
});

test("tag release workflow builds and publishes a traceable checked Linux artifact", () => {
  const workflow = read(".github/workflows/release.yml");
  assert.match(workflow, /tags:\s*\n\s*- ['"]v\*\.\*\.\*['"]/);
  assert.match(workflow, /runs-on: ubuntu-24\.04/);
  assert.match(workflow, /contents: write/);
  assert.doesNotMatch(workflow, /uses: [^\n]+@(master|main)\b/);
  assert.doesNotMatch(workflow, /uses: [^\n]+@v\d+\s*$/m);
  assert.match(workflow, /ref: \$\{\{ github\.ref \}\}/);
  assert.match(workflow, /git rev-parse HEAD/);
  assert.match(workflow, /\^v\[0-9\]\+\\\.\[0-9\]\+\\\.\[0-9\]\+\$/);
  assert.match(workflow, /toolchain: 1\.93\.1/);
  assert.match(workflow, /node-version: 22\.22\.0/);
  assert.equal((workflow.match(/\bnpm ci\b/g) ?? []).length, 2);
  assert.match(workflow, /make check/);
  assert.match(workflow, /cargo build --release --locked --target x86_64-unknown-linux-gnu/);
  assert.match(workflow, /glim-\$\{VERSION\}-\$\{TARGET\}\.tar\.gz/);
  assert.match(workflow, /sha256sum "\$\{ARTIFACT\}" > "\$\{ARTIFACT\}\.sha256"/);
  assert.match(workflow, /tag: \$\{\{ github\.ref_name \}\}/);
  assert.match(workflow, /commit: \$\{\{ github\.sha \}\}/);
  assert.match(workflow, /files: \|[\s\S]*\.tar\.gz[\s\S]*\.tar\.gz\.sha256/);
});

test("ordinary CI and make check enforce the release contract without publishing", () => {
  const makefile = read("Makefile");
  assert.match(makefile, /^release-contract-check:\n\tnode tests\/release-contract\.mjs$/m);
  assert.match(makefile, /^check: .*release-contract-check/m);

  const ci = read(".github/workflows/ci.yml");
  assert.match(ci, /run: make check/);
  assert.doesNotMatch(ci, /action-gh-release|upload-release-asset|gh release/);
});

test("public operations documentation defines install, upgrade, backup, removal, and checksum boundaries", () => {
  const readme = read("README.md");
  assert.match(readme, /docs\/operations\.md/);

  const docs = read("docs/operations.md");
  for (const required of [
    "cargo install --path . --locked",
    "pi install git:github.com/ryan-ressmeyer/glim@<tag-or-commit>",
    "glim service stop",
    "metadata.sqlite3",
    "access-token",
    "TLS",
    "sha256sum --check",
    "DESTRUCTIVE",
  ]) {
    assert.ok(docs.includes(required), `operations documentation must contain ${required}`);
  }
  assert.doesNotMatch(docs, /rm\s+-rf\s+[^\n]*\*/);
  assert.match(docs, /newer than supported/i);
  assert.match(docs, /downgrade/i);
});
