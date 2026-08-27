import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const skillDir = join(root, "integrations/generic-skill/glim");
const skillPath = join(skillDir, "SKILL.md");
const referencePath = join(skillDir, "references/cli-contract.md");
assert(existsSync(skillPath), "generic Glimse skill package is missing");
assert(existsSync(referencePath), "generic Glimse CLI reference is missing");
const skill = readFileSync(skillPath, "utf8");
const reference = readFileSync(referencePath, "utf8");

function frontmatter(markdown) {
  const match = markdown.match(/^---\n([\s\S]*?)\n---\n/);
  assert(match, "SKILL.md must start with YAML frontmatter");
  return Object.fromEntries(match[1].split("\n").map((line) => {
    const separator = line.indexOf(":");
    assert(separator > 0, `invalid frontmatter line: ${line}`);
    return [line.slice(0, separator).trim(), line.slice(separator + 1).trim().replace(/^['"]|['"]$/g, "")];
  }));
}

const metadata = frontmatter(skill);
assert.equal(metadata.name, "glim");
assert.equal(metadata.name, skillDir.split("/").at(-1));
assert.match(metadata.name, /^(?!-)(?!.*--)[a-z0-9-]{1,64}(?<!-)$/);
assert(metadata.description.startsWith("Use when"));
assert(metadata.description.length <= 1024);
assert(!("disable-model-invocation" in metadata), "generic workflow must remain model-invoked");

const markdownFiles = [skillPath, referencePath];
for (const path of markdownFiles) {
  const text = readFileSync(path, "utf8");
  for (const match of text.matchAll(/\[[^\]]+\]\(([^)]+)\)/g)) {
    const target = match[1].split("#")[0];
    if (!target || /^[a-z]+:/i.test(target) || target.startsWith("#")) continue;
    const resolved = resolve(dirname(path), target);
    assert(statSync(resolved).isFile(), `${relative(root, path)} has broken link ${target}`);
    if (path === referencePath && target.endsWith(".md")) {
      assert.fail("required context may not be hidden behind a second Markdown reference");
    }
  }
}
assert(skill.includes("[CLI contract](references/cli-contract.md)"));

for (const section of skill.split(/^### /m).slice(1)) {
  if (/^Step \d+\b/.test(section)) {
    assert(section.includes("**Complete when:**"), `workflow step lacks completion criterion: ${section.split("\n")[0]}`);
  }
}

function validate(value, schema, location = "$") {
  if ("const" in schema) assert.deepEqual(value, schema.const, `${location} does not match const`);
  const types = Array.isArray(schema.type) ? schema.type : schema.type ? [schema.type] : [];
  if (types.length) {
    const actual = value === null ? "null" : Array.isArray(value) ? "array" : Number.isInteger(value) ? "integer" : typeof value;
    assert(types.includes(actual) || (actual === "integer" && types.includes("number")), `${location} has type ${actual}, expected ${types}`);
  }
  if (typeof value === "string") {
    if (schema.minLength !== undefined) assert(value.length >= schema.minLength, `${location} is too short`);
    if (schema.pattern) assert(new RegExp(schema.pattern).test(value), `${location} does not match pattern`);
  }
  if (Number.isInteger(value) && schema.minimum !== undefined) assert(value >= schema.minimum, `${location} is below minimum`);
  if (Array.isArray(value)) {
    if (schema.minItems !== undefined) assert(value.length >= schema.minItems, `${location} has too few items`);
    if (schema.maxItems !== undefined) assert(value.length <= schema.maxItems, `${location} has too many items`);
    value.forEach((item, index) => validate(item, schema.items, `${location}[${index}]`));
  } else if (value && typeof value === "object") {
    for (const key of schema.required ?? []) assert(Object.hasOwn(value, key), `${location} lacks ${key}`);
    if (schema.additionalProperties === false) {
      for (const key of Object.keys(value)) assert(Object.hasOwn(schema.properties, key), `${location} has unknown property ${key}`);
    }
    for (const [key, child] of Object.entries(schema.properties ?? {})) {
      if (Object.hasOwn(value, key)) validate(value[key], child, `${location}.${key}`);
    }
  }
}

const publishSchema = JSON.parse(readFileSync(join(root, "docs/cli-publish-v1.schema.json"), "utf8"));
const fixtureDir = join(skillDir, "assets");
const fixtures = readdirSync(fixtureDir).filter((name) => name.endsWith(".json"));
assert(fixtures.length >= 2, "expected publication and revision fixtures");
for (const name of fixtures) validate(JSON.parse(readFileSync(join(fixtureDir, name), "utf8")), publishSchema, name);
const revision = JSON.parse(readFileSync(join(fixtureDir, "revision.json"), "utf8"));
assert.equal(revision.predecessor_post_id, 48);
assert.equal(revision.external_session_key, "pi-run-77");
assert.equal(revision.integration_namespace, "pi");
assert(revision.project_label && revision.working_directory.startsWith("/"));

assert(reference.includes('glim publish --json < "$manifest"'), "canonical example must redirect a safely written manifest to stdin");
assert.match(reference, /JSON serializer|structured file tool/);
assert.doesNotMatch(reference, /echo\s+.*\|\s*glim publish/);
assert.doesNotMatch(reference, /glim publish[^\n]*--json[^\n]*['"]\{.*\$\{/);
assert.match(skill, /Never watch directories or scan for artifacts automatically/);
assert.match(skill, /Do not publish routine source changes, diffs, test output, logs, terminal transcripts/);
assert.match(skill, /mandatory barrier before another publication/i);
assert.match(skill, /direct request to retry do not bypass this barrier/i);
assert.match(reference, /confirmed state inspection must precede any new publication/i);
assert.match(reference, /does not support an idempotency key/i);
assert.match(reference, /glim close \$PUBLIC_SESSION_ID/);
assert.match(reference, /result\.session\.public_id/);

const cliSource = readFileSync(join(root, "src/cli.rs"), "utf8");
const apiSource = readFileSync(join(root, "src/api.rs"), "utf8");
const makefile = readFileSync(join(root, "Makefile"), "utf8");
assert.match(makefile, /^check:.*generic-skill-check/m, "make check must include the generic skill checks");
assert.match(makefile, /^generic-skill-check:\n\tnode tests\/generic-skill\.mjs$/m);
for (const command of ["publish", "health", "status", "show", "close", "list", "open", "service"]) {
  assert(cliSource.includes(`"${command}"`), `documented command ${command} missing from CLI`);
}
for (const code of [
  "daemon_unavailable", "configuration_error", "authentication_required", "invalid_credentials",
  "upload_limit_exceeded", "storage_limit_exceeded", "artifact_classification_failed",
  "malformed_daemon_response", "browser_launch_failed", "validation_error", "asset_collection_error",
]) {
  assert(reference.includes(`\`${code}\``), `reference omits stable code ${code}`);
  assert(cliSource.includes(`"${code}"`) || apiSource.includes(`"${code}"`), `documented code ${code} missing from implementation`);
}

console.log(`generic skill checks passed (${fixtures.length} schema-valid fixtures)`);
