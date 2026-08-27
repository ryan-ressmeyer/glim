import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawn } from "node:child_process";

const root = resolve(import.meta.dirname, "../../..");
const temp = await mkdtemp(join(tmpdir(), "glim-pi-load-"));
const observer = join(temp, "observer.ts");
await writeFile(observer, `
export default function (pi) {
  pi.registerCommand("pi-package-inspect", {
    description: "test observer",
    handler: async (_args, ctx) => ctx.ui.notify(JSON.stringify({tools: pi.getAllTools().map(t => t.name)}), "info"),
  });
  pi.registerCommand("pi-package-reload", {
    description: "test reload",
    handler: async (_args, ctx) => { await ctx.reload(); return; },
  });
}
`);

const child = spawn("pi", ["--mode", "rpc", "--no-session", "--approve", "--offline", "--no-context-files", "-e", root, "-e", observer], {
  cwd: root,
  env: { ...process.env, PI_CODING_AGENT_DIR: join(temp, "config"), PI_CODING_AGENT_SESSION_DIR: join(temp, "sessions"), PI_OFFLINE: "1" },
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
  send({ id: "commands", type: "get_commands" });
  const commands = await waitFor(e => e.type === "response" && e.id === "commands");
  assert.equal(commands.success, true);
  const names = commands.data.commands.map(command => command.name);
  for (const name of ["glim-feed", "glim-status", "glim-close", "skill:glim", "pi-package-inspect", "pi-package-reload"]) assert.ok(names.includes(name), `missing ${name}`);

  send({ id: "inspect-1", type: "prompt", message: "/pi-package-inspect" });
  const first = await waitFor(e => e.type === "extension_ui_request" && e.method === "notify" && e.message?.includes('"tools"'));
  assert.ok(JSON.parse(first.message).tools.includes("glim_publish"));

  send({ id: "reload", type: "prompt", message: "/pi-package-reload" });
  await waitFor(e => e.type === "response" && e.id === "reload" && e.success === true);
  events.length = 0;
  send({ id: "inspect-2", type: "prompt", message: "/pi-package-inspect" });
  const second = await waitFor(e => e.type === "extension_ui_request" && e.method === "notify" && e.message?.includes('"tools"'));
  assert.ok(JSON.parse(second.message).tools.includes("glim_publish"));
  assert.equal(stderr.trim(), "", `Pi emitted startup/reload warnings: ${stderr}`);
  process.stdout.write("Pi loaded glim_publish, three commands, and the glim skill; reload preserved registration.\n");
} finally {
  child.stdin.end();
  child.kill("SIGTERM");
}
