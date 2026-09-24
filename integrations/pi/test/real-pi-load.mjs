import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";

const root = resolve(import.meta.dirname, "../../..");
const temp = await mkdtemp(join(tmpdir(), "glim-pi-load-"));
const observer = join(temp, "observer.ts");
await writeFile(observer, `
export default function (pi) {
  pi.on("before_provider_request", () => { throw new Error("provider call was not expected"); });
  pi.registerCommand("pi-package-inspect", {
    description: "test observer",
    handler: async (_args, ctx) => ctx.ui.notify(JSON.stringify({
      activeTools: pi.getActiveTools(),
      allTools: pi.getAllTools().map(t => t.name),
      systemPrompt: ctx.getSystemPrompt(),
    }), "info"),
  });
  pi.registerCommand("pi-package-reload", {
    description: "test reload",
    handler: async (_args, ctx) => { await ctx.reload(); return; },
  });
}
`);

const availability = join(temp, "availability");
await writeFile(availability, "unavailable");
const bin = join(temp, "bin");
await mkdir(bin);
const fakeGlim = join(bin, "glim");
await writeFile(fakeGlim, `#!/usr/bin/env node
const { readFileSync } = require("node:fs");
process.stdin.resume();
process.stdin.on("end", () => {
  if (process.argv[2] !== "status") process.exit(9);
  if (readFileSync(${JSON.stringify(availability)}, "utf8").trim() !== "healthy") {
    process.stdout.write(JSON.stringify({schema_version:1,ok:false,error:{code:"daemon_unavailable",message:"offline",details:{}}}));
    process.exitCode = 3;
    return;
  }
  process.stdout.write(JSON.stringify({schema_version:1,ok:true,result:{
    ok:true,version:"0.1.0",staging_bytes_in_use:0,finalized_unique_blob_bytes:0,
    max_upload_bytes:536870912,max_staging_bytes:2147483648,max_finalized_blob_bytes:21474836480,
    max_concurrent_publications:4,active_sessions:0,sessions_due_for_purge:0,queued_blob_deletions:0,
    retention_seconds:604800,cleanup_interval_seconds:3600
  }}));
});
`);
await chmod(fakeGlim, 0o755);

const piEnv = {
  ...process.env,
  PATH: `${bin}${delimiter}${process.env.PATH ?? ""}`,
  HOME: temp,
  PI_CODING_AGENT_DIR: join(temp, "config"),
  PI_CODING_AGENT_SESSION_DIR: join(temp, "sessions"),
  PI_OFFLINE: "1",
};
const common = ["--no-session", "--approve", "--offline", "--no-context-files", "-e", root, "-e", observer];
const print = spawnSync("pi", ["-p", ...common, "/glim-feed"], {
  cwd: root,
  env: piEnv,
  encoding: "utf8",
});
assert.equal(print.status, 0, print.stderr);
assert.equal(print.stdout, "");
assert.match(print.stderr, /No confirmed open Glimse publication exists/);
assert.doesNotMatch(print.stderr, /provider call was not expected/);

const json = spawnSync("pi", ["--mode", "json", ...common, "/glim-feed"], {
  cwd: root,
  env: piEnv,
  encoding: "utf8",
});
assert.equal(json.status, 0, json.stderr);
assert.equal(json.stderr, "");
const jsonEvents = json.stdout.trim().split("\n").map(line => JSON.parse(line));
assert.ok(jsonEvents.some(event => event.type === "message_end" && event.message?.role === "custom" &&
  event.message?.customType === "glimse-pi-command-v1" && event.message?.content?.includes("No confirmed open Glimse publication exists")));

const child = spawn("pi", ["--mode", "rpc", ...common], {
  cwd: root,
  env: piEnv,
  stdio: ["pipe", "pipe", "pipe"],
});
let stderr = "";
child.stderr.setEncoding("utf8");
child.stderr.on("data", chunk => stderr += chunk);
let buffer = "";
const events = [];
const waiters = [];
child.stdout.setEncoding("utf8");
child.stdout.on("data", chunk => {
  buffer += chunk;
  for (;;) {
    const at = buffer.indexOf("\n");
    if (at < 0) break;
    const line = buffer.slice(0, at).replace(/\r$/, "");
    buffer = buffer.slice(at + 1);
    if (!line) continue;
    const event = JSON.parse(line);
    events.push(event);
    for (const waiter of [...waiters]) if (waiter.predicate(event)) { waiters.splice(waiters.indexOf(waiter), 1); waiter.resolve(event); }
  }
});
const waitFor = (predicate, timeout = 15000) => new Promise((resolveWait, reject) => {
  const found = events.find(predicate);
  if (found) return resolveWait(found);
  const waiter = { predicate, resolve: resolveWait };
  waiters.push(waiter);
  setTimeout(() => {
    const index = waiters.indexOf(waiter);
    if (index >= 0) waiters.splice(index, 1);
    reject(new Error(`timed out waiting for Pi RPC event; stderr=${stderr}`));
  }, timeout).unref();
});
const send = value => child.stdin.write(`${JSON.stringify(value)}\n`);

try {
  send({ id: "commands-1", type: "get_commands" });
  const commands1 = await waitFor(e => e.type === "response" && e.id === "commands-1");
  assert.equal(commands1.success, true);
  const names1 = commands1.data.commands.map(command => command.name);
  for (const name of ["glim-feed", "glim-status", "glim-close", "pi-package-inspect", "pi-package-reload"]) assert.ok(names1.includes(name), `missing ${name}`);
  assert.ok(!names1.includes("skill:glim"), "unavailable Glimse advertised its skill");

  send({ id: "inspect-1", type: "prompt", message: "/pi-package-inspect" });
  const firstEvent = await waitFor(e => e.type === "extension_ui_request" && e.method === "notify" && e.message?.includes('"activeTools"'));
  const first = JSON.parse(firstEvent.message);
  assert.ok(first.allTools.includes("glim_publish"), "publication tool was not registered");
  assert.ok(!first.activeTools.includes("glim_publish"), "unavailable publication tool was active");
  assert.doesNotMatch(first.systemPrompt, /glim_publish|routine diffs/, "unavailable publication guidance leaked into the prompt");

  await writeFile(availability, "healthy");
  send({ id: "reload-healthy", type: "prompt", message: "/pi-package-reload" });
  await waitFor(e => e.type === "response" && e.id === "reload-healthy" && e.success === true);
  events.length = 0;
  send({ id: "commands-2", type: "get_commands" });
  const commands2 = await waitFor(e => e.type === "response" && e.id === "commands-2");
  assert.ok(commands2.data.commands.some(command => command.name === "skill:glim"), "healthy Glimse did not advertise its skill");
  send({ id: "inspect-2", type: "prompt", message: "/pi-package-inspect" });
  const secondEvent = await waitFor(e => e.type === "extension_ui_request" && e.method === "notify" && e.message?.includes('"activeTools"'));
  const second = JSON.parse(secondEvent.message);
  assert.ok(second.activeTools.includes("glim_publish"), "healthy publication tool was inactive");
  assert.match(second.systemPrompt, /glim_publish/, "healthy publication guidance was absent from the prompt");

  await writeFile(availability, "unavailable");
  send({ id: "reload-unavailable", type: "prompt", message: "/pi-package-reload" });
  await waitFor(e => e.type === "response" && e.id === "reload-unavailable" && e.success === true);
  events.length = 0;
  send({ id: "commands-3", type: "get_commands" });
  const commands3 = await waitFor(e => e.type === "response" && e.id === "commands-3");
  assert.ok(!commands3.data.commands.some(command => command.name === "skill:glim"), "unhealthy reload retained the skill");
  send({ id: "inspect-3", type: "prompt", message: "/pi-package-inspect" });
  const thirdEvent = await waitFor(e => e.type === "extension_ui_request" && e.method === "notify" && e.message?.includes('"activeTools"'));
  const third = JSON.parse(thirdEvent.message);
  assert.ok(!third.activeTools.includes("glim_publish"), "unhealthy reload retained the publication tool");
  assert.doesNotMatch(third.systemPrompt, /glim_publish|routine diffs/, "unhealthy reload retained publication guidance");

  assert.equal(stderr.trim(), "", `Pi emitted startup/reload warnings: ${stderr}`);
  process.stdout.write("Pi gated glim_publish and the glim skill across unavailable, healthy, and unavailable reload states.\n");
} finally {
  child.stdin.end();
  child.kill("SIGTERM");
}
