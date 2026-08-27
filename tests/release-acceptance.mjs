import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { access, chmod, copyFile, mkdir, mkdtemp, readFile, realpath, rm, stat, writeFile } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repository = fileURLToPath(new URL("../", import.meta.url));
const evidencePath = path.join(repository, "docs", "release-acceptance.md");
const target = "x86_64-unknown-linux-gnu";
const models = {
  codex: "openai-codex/gpt-5.6-sol",
  haiku: "openrouter/anthropic/claude-haiku-4.5",
};
const matrix = [
  { model: models.codex, mode: "json" },
  { model: models.codex, mode: "rpc" },
  { model: models.haiku, mode: "print" },
  { model: models.haiku, mode: "rpc" },
];
const limits = {
  totalMs: integerEnvironment("GLIM_ACCEPT_TIMEOUT_MS", 30 * 60_000),
  modelMs: integerEnvironment("GLIM_ACCEPT_MODEL_TIMEOUT_MS", 4 * 60_000),
  outputBytes: integerEnvironment("GLIM_ACCEPT_MAX_OUTPUT_BYTES", 2 * 1024 * 1024),
  turns: 3,
};
const enabled = process.env.GLIM_RUN_LIVE_ACCEPTANCE === "1";
if (!enabled) throw new Error("live release acceptance is opt-in; set GLIM_RUN_LIVE_ACCEPTANCE=1");

const artifactNames = [
  "report.md", "support.png", "diagram.svg", "notes.txt", "data.json", "table.csv",
  "safe.html", "document.pdf", "tone.wav", "clip.mp4",
];
const rendererNames = ["image", "svg", "markdown", "text", "json", "csv", "html", "pdf", "audio", "video"];
const root = await mkdtemp(path.join(os.tmpdir(), "glim-release-acceptance-"));
const cargoRoot = path.join(root, "cargo-root");
const workspace = path.join(root, "workspace");
const sessions = path.join(root, "pi-sessions");
const store = path.join(root, "store");
const configPath = path.join(root, "config.json");
const tokenPath = path.join(root, "access-token");
const browserProfile = path.join(root, "chromium-profile");
const token = randomBytes(32).toString("hex");
const results = new Map(matrix.map(({ model, mode }) => [`${model}|${mode}`, "NOT RUN"]));
const renderers = new Map(rendererNames.map((renderer) => [renderer, "NOT RUN"]));
let localResult = "FAIL";
let closureResult = "FAIL";
let daemon;
let browser;
let cdp;
let fatal;

function integerEnvironment(name, fallback) {
  const raw = process.env[name];
  if (raw === undefined) return fallback;
  if (!/^\d+$/.test(raw) || Number(raw) <= 0 || !Number.isSafeInteger(Number(raw))) throw new Error(`${name} must be a positive integer`);
  return Number(raw);
}

function sanitizedBlocker(error) {
  const code = typeof error?.code === "string" ? error.code : "acceptance_error";
  if (error?.timedOut) return `${code} (bounded timeout)`;
  if (Number.isInteger(error?.exitCode)) return `${code} (exit ${error.exitCode})`;
  return code;
}

function assertTokenAbsent(text, label) {
  if (text.includes(token)) throw Object.assign(new Error(`${label} exposed the daemon token`), { code: "token_exposure" });
}

async function reservePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => server.listen(0, "127.0.0.1", resolve).once("error", reject));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("could not allocate loopback port");
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  return address.port;
}

async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill("SIGTERM");
  for (let attempt = 0; attempt < 50; attempt += 1) {
    if (child.exitCode !== null || child.signalCode !== null) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  child.kill("SIGKILL");
  await new Promise((resolve) => child.once("close", resolve));
}

function spawnBounded(command, args, { cwd = repository, env = process.env, input, timeoutMs = limits.modelMs } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, env, stdio: ["pipe", "pipe", "pipe"] });
    const stdout = [];
    const stderr = [];
    let bytes = 0;
    let settled = false;
    const fail = (error) => {
      if (settled) return;
      settled = true;
      child.kill("SIGKILL");
      reject(error);
    };
    const collect = (destination) => (chunk) => {
      bytes += chunk.length;
      if (bytes > limits.outputBytes) return fail(Object.assign(new Error("subprocess output exceeded limit"), { code: "output_limit" }));
      destination.push(chunk);
    };
    child.stdout.on("data", collect(stdout));
    child.stderr.on("data", collect(stderr));
    child.once("error", (error) => fail(Object.assign(error, { code: "spawn_error" })));
    const timeout = setTimeout(() => fail(Object.assign(new Error("subprocess timed out"), { code: "subprocess_timeout", timedOut: true })), timeoutMs);
    child.once("close", (exitCode, signal) => {
      clearTimeout(timeout);
      if (settled) return;
      settled = true;
      const output = Buffer.concat(stdout).toString("utf8");
      const errors = Buffer.concat(stderr).toString("utf8");
      assertTokenAbsent(output, command);
      assertTokenAbsent(errors, command);
      if (exitCode !== 0) return reject(Object.assign(new Error(`${command} failed`), { code: "process_exit", exitCode, signal }));
      resolve({ stdout: output, stderr: errors });
    });
    if (input === undefined) child.stdin.end();
    else child.stdin.end(input);
  });
}

function daemonEnvironment(candidateBin) {
  const env = {
    ...process.env,
    PATH: `${candidateBin}${path.delimiter}${process.env.PATH ?? ""}`,
    GLIM_CONFIG: configPath,
    PI_SKIP_VERSION_CHECK: "1",
    PI_TELEMETRY: "0",
  };
  for (const name of [
    "GLIM_STORE_ROOT", "GLIM_BIND", "GLIM_ACCESS_MODE", "GLIM_TOKEN_FILE", "GLIM_PUBLIC_ORIGIN",
    "GLIM_TLS_CERTIFICATE", "GLIM_TLS_PRIVATE_KEY", "GLIM_TRUSTED_PROXY_IPS", "GLIM_MAX_UPLOAD_BYTES",
    "GLIM_MAX_FINALIZED_BLOB_BYTES", "GLIM_DAEMON_URL", "GLIM_LOG_LEVEL",
  ]) delete env[name];
  env.GLIM_CONFIG = configPath;
  return env;
}

async function installCandidate() {
  await mkdir(cargoRoot, { recursive: true });
  const explicit = process.env.GLIM_ACCEPT_CANDIDATE;
  if (explicit) {
    const candidate = await realpath(explicit);
    if (!(await stat(candidate)).isFile()) throw new Error("GLIM_ACCEPT_CANDIDATE is not a file");
    const bin = path.join(cargoRoot, "bin");
    await mkdir(bin, { recursive: true });
    await copyFile(candidate, path.join(bin, "glim"));
    await chmod(path.join(bin, "glim"), 0o755);
  } else {
    await spawnBounded("cargo", ["install", "--path", repository, "--locked", "--root", cargoRoot, "--force"], { timeoutMs: limits.totalMs });
  }
  const candidate = path.join(cargoRoot, "bin", "glim");
  await access(candidate);
  return { candidate, candidateBin: path.dirname(candidate) };
}

async function packageFixture(candidate) {
  const cargo = await readFile(path.join(repository, "Cargo.toml"), "utf8");
  const version = /^version = "([^"]+)"$/m.exec(cargo)?.[1];
  assert(version, "Cargo version unavailable");
  const tag = `v${version}`;
  const packageName = `glim-${tag}-${target}`;
  const dist = path.join(root, "dist");
  const packageDirectory = path.join(dist, packageName);
  await mkdir(packageDirectory, { recursive: true });
  await copyFile(candidate, path.join(packageDirectory, "glim"));
  await chmod(path.join(packageDirectory, "glim"), 0o755);
  await copyFile(path.join(repository, "LICENSE"), path.join(packageDirectory, "LICENSE"));
  await copyFile(path.join(repository, "README.md"), path.join(packageDirectory, "README.md"));
  const archive = path.join(dist, `${packageName}.tar.gz`);
  await spawnBounded("tar", ["-C", dist, "-czf", archive, packageName], { timeoutMs: 60_000 });
  const checksum = createHash("sha256").update(await readFile(archive)).digest("hex");
  const checksumPath = `${archive}.sha256`;
  await writeFile(checksumPath, `${checksum}  ${path.basename(archive)}\n`);
  await spawnBounded("sha256sum", ["--check", path.basename(checksumPath)], { cwd: dist, timeoutMs: 30_000 });
  const listing = (await spawnBounded("tar", ["-tzf", archive], { timeoutMs: 30_000 })).stdout.trim().split("\n").sort();
  assert.deepEqual(listing, [
    `${packageName}/`, `${packageName}/LICENSE`, `${packageName}/README.md`, `${packageName}/glim`,
  ].sort());
}

function pdfBytes() {
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    "<< /Length 42 >>\nstream\nBT /F1 14 Tf 20 50 Td (Glimse PDF) Tj ET\nendstream",
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
  ];
  let body = "%PDF-1.4\n";
  const offsets = [0];
  objects.forEach((object, index) => {
    offsets.push(Buffer.byteLength(body));
    body += `${index + 1} 0 obj\n${object}\nendobj\n`;
  });
  const xref = Buffer.byteLength(body);
  body += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  body += offsets.slice(1).map((offset) => `${String(offset).padStart(10, "0")} 00000 n \n`).join("");
  body += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(body);
}

function wavBytes() {
  const samples = 800;
  const buffer = Buffer.alloc(44 + samples * 2);
  buffer.write("RIFF", 0); buffer.writeUInt32LE(buffer.length - 8, 4); buffer.write("WAVEfmt ", 8);
  buffer.writeUInt32LE(16, 16); buffer.writeUInt16LE(1, 20); buffer.writeUInt16LE(1, 22);
  buffer.writeUInt32LE(8000, 24); buffer.writeUInt32LE(16000, 28); buffer.writeUInt16LE(2, 32); buffer.writeUInt16LE(16, 34);
  buffer.write("data", 36); buffer.writeUInt32LE(samples * 2, 40);
  for (let index = 0; index < samples; index += 1) buffer.writeInt16LE(Math.round(Math.sin(index * Math.PI / 10) * 2000), 44 + index * 2);
  return buffer;
}

async function createFixtures() {
  await mkdir(workspace, { recursive: true });
  const supportPng = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=", "base64");
  await writeFile(path.join(workspace, "support.png"), supportPng);
  await writeFile(path.join(workspace, "report.md"), "# Acceptance report\n\n![Collected support image](support.png)\n");
  await writeFile(path.join(workspace, "diagram.svg"), '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"><rect width="64" height="32" fill="#2563eb"/><text x="5" y="21" fill="white">Glim</text></svg>\n');
  await writeFile(path.join(workspace, "notes.txt"), "deterministic acceptance text\n");
  await writeFile(path.join(workspace, "data.json"), '{"accepted":true,"count":2}\n');
  await writeFile(path.join(workspace, "table.csv"), "name,value\nalpha,1\nbeta,2\n");
  await writeFile(path.join(workspace, "safe.css"), "body { color: rgb(30, 41, 59); }\n");
  await writeFile(path.join(workspace, "safe.html"), '<!doctype html><link rel="stylesheet" href="safe.css"><h1>Safe HTML</h1><script>parent.postMessage("must-not-run", "*")</script>\n');
  await writeFile(path.join(workspace, "document.pdf"), pdfBytes());
  await writeFile(path.join(workspace, "tone.wav"), wavBytes());
  await spawnBounded("ffmpeg", ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=blue:s=32x32:d=0.2:r=5", "-an", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-movflags", "+faststart", "-y", path.join(workspace, "clip.mp4")], { timeoutMs: 60_000 });
}

function publicationPrompt(files, predecessor) {
  const specifications = files.map((name) => ({ path: name, caption: `Acceptance ${name}` }));
  const revision = predecessor === undefined ? "" : ` and predecessor_post_id=${predecessor}`;
  return `Call glim_publish exactly once with title "Release acceptance", commentary "Inspect the selected acceptance artifacts."${revision}, open=false, and this exact ordered files array: ${JSON.stringify(specifications)}. Do not call any other tool. After the tool returns, report the confirmed post and feed URLs concisely.`;
}

function commonPiArgs(model, sessionFile) {
  return [
    "--model", model, "--thinking", "minimal", "--session", sessionFile, "--session-dir", sessions,
    "--no-extensions", "-e", repository, "--no-skills", "--no-prompt-templates", "--no-themes",
    "--no-context-files", "--no-builtin-tools", "--tools", "glim_publish", "--no-approve",
    "--system-prompt", "Follow the publication request exactly. Use the only available tool once, then stop.",
  ];
}

function parseJsonLines(text) {
  return text.split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

function confirmedPublication(events, label) {
  const turns = events.filter((event) => event.type === "turn_start").length;
  assert(turns <= limits.turns, `${label} exceeded ${limits.turns} turns`);
  const calls = events.filter((event) => event.type === "tool_execution_end" && event.toolName === "glim_publish");
  assert.equal(calls.length, 1, `${label} did not execute glim_publish exactly once`);
  const ambiguity = calls[0].result?.details?.publicationMayHaveSucceeded === true
    || calls[0].result?.details?.publication_may_have_succeeded === true;
  if (ambiguity) throw Object.assign(new Error(`${label} returned ambiguous publication state`), { code: "ambiguous_publication" });
  assert.equal(calls[0].isError, false, `${label} publication tool failed`);
  const details = calls[0].result?.details;
  assert(typeof details?.public_session_id === "string" && details.public_session_id.length > 0);
  assert(Number.isInteger(details?.post_id) && details.post_id > 0);
  assert.match(details.viewer_url, /^http:\/\/127\.0\.0\.1:\d+\/sessions\/[A-Za-z0-9]+$/);
  assert.match(details.post_url, /^http:\/\/127\.0\.0\.1:\d+\/sessions\/[A-Za-z0-9]+#post-\d+$/);
  return details;
}

async function piJsonPublish(env, model, sessionFile, files, predecessor) {
  const { stdout } = await spawnBounded("pi", [...commonPiArgs(model, sessionFile), "--mode", "json", publicationPrompt(files, predecessor)], { cwd: workspace, env });
  return confirmedPublication(parseJsonLines(stdout), `${model} JSON`);
}

async function piPrintPublish(env, model, sessionFile, files) {
  const { stdout } = await spawnBounded("pi", [...commonPiArgs(model, sessionFile), "--print", publicationPrompt(files)], { cwd: workspace, env });
  assert.match(stdout, /https?:\/\/127\.0\.0\.1:\d+\/sessions\//);
  const entries = parseJsonLines(await readFile(sessionFile, "utf8"));
  return confirmedPublication(entries.flatMap((entry) => {
    if (entry.type !== "message") return [];
    if (entry.message?.role === "toolResult" && entry.message.toolName === "glim_publish") {
      return [{ type: "tool_execution_end", toolName: "glim_publish", isError: entry.message.isError === true, result: entry.message }];
    }
    if (entry.message?.role === "assistant") return [{ type: "turn_start" }];
    return [];
  }), `${model} print`);
}

class RpcPi {
  constructor(env, model, sessionFile) {
    this.events = [];
    this.waiters = [];
    this.stderr = "";
    this.buffer = "";
    this.child = spawn("pi", [...commonPiArgs(model, sessionFile), "--mode", "rpc"], { cwd: workspace, env, stdio: ["pipe", "pipe", "pipe"] });
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk) => {
      this.stderr += chunk;
      if (Buffer.byteLength(this.stderr) > limits.outputBytes) this.child.kill("SIGKILL");
    });
    this.child.stdout.setEncoding("utf8");
    this.child.stdout.on("data", (chunk) => this.consume(chunk));
  }
  consume(chunk) {
    this.buffer += chunk;
    if (Buffer.byteLength(this.buffer) > limits.outputBytes) this.child.kill("SIGKILL");
    for (;;) {
      const newline = this.buffer.indexOf("\n");
      if (newline < 0) return;
      const line = this.buffer.slice(0, newline).replace(/\r$/, "");
      this.buffer = this.buffer.slice(newline + 1);
      if (!line) continue;
      assertTokenAbsent(line, "Pi RPC");
      const event = JSON.parse(line);
      this.events.push(event);
      for (const waiter of [...this.waiters]) {
        if (!waiter.predicate(event, this.events.length - 1)) continue;
        this.waiters.splice(this.waiters.indexOf(waiter), 1);
        clearTimeout(waiter.timeout);
        waiter.resolve(event);
      }
    }
  }
  send(value) { this.child.stdin.write(`${JSON.stringify(value)}\n`); }
  wait(predicate, timeoutMs = limits.modelMs) {
    const index = this.events.findIndex(predicate);
    if (index >= 0) return Promise.resolve(this.events[index]);
    return new Promise((resolve, reject) => {
      const waiter = { predicate, resolve, timeout: undefined };
      waiter.timeout = setTimeout(() => {
        this.waiters.splice(this.waiters.indexOf(waiter), 1);
        reject(Object.assign(new Error("Pi RPC timed out"), { code: "rpc_timeout", timedOut: true }));
      }, timeoutMs);
      this.waiters.push(waiter);
    });
  }
  async start() {
    this.send({ id: "retry", type: "set_auto_retry", enabled: false });
    const response = await this.wait((event) => event.type === "response" && event.id === "retry");
    assert.equal(response.success, true);
  }
  async prompt(message, id) {
    const start = this.events.length;
    this.send({ id, type: "prompt", message });
    const accepted = await this.wait((event) => event.type === "response" && event.id === id);
    assert.equal(accepted.success, true);
    await this.wait((event, index) => index >= start && event.type === "agent_settled");
    return this.events.slice(start);
  }
  async command(command, id, expectedFragment) {
    const start = this.events.length;
    this.send({ id, type: "prompt", message: command });
    const accepted = await this.wait((event) => event.type === "response" && event.id === id);
    assert.equal(accepted.success, true);
    await this.wait((event, index) => index >= start
      && event.type === "extension_ui_request"
      && event.method === "notify"
      && event.message?.includes(expectedFragment));
    return this.events.slice(start);
  }
  async close() {
    this.child.stdin.end();
    await stop(this.child);
    assertTokenAbsent(this.stderr, "Pi RPC stderr");
  }
}

async function cli(env, args, input) {
  let stdout;
  try {
    ({ stdout } = await spawnBounded("glim", args, { cwd: workspace, env, input: input === undefined ? undefined : JSON.stringify(input), timeoutMs: 120_000 }));
  } catch (error) {
    if (error?.code === "process_exit") error.code = `cli_${args[0]}_exit`;
    throw error;
  }
  const envelope = JSON.parse(stdout);
  if (!envelope.ok) throw Object.assign(new Error("glim CLI rejected request"), { code: envelope.error?.code ?? "cli_error" });
  return envelope.result;
}

function manifest(externalKey, files, predecessor) {
  return {
    schema_version: 1,
    integration_namespace: "release-acceptance",
    external_session_key: externalKey,
    project_label: "release-acceptance-workspace",
    working_directory: workspace,
    title: predecessor ? "Release acceptance revision" : "Release acceptance",
    commentary: "Inspect the selected acceptance artifacts.",
    ...(predecessor ? { predecessor_post_id: predecessor } : {}),
    files: files.map((name) => ({ source_path: path.join(workspace, name), caption: `Acceptance ${name}` })),
  };
}

async function directPublish(env, key, files, predecessor) {
  const result = await cli(env, ["publish", "--json"], manifest(key, files, predecessor));
  return {
    public_session_id: result.session.public_id,
    post_id: result.post.id,
    viewer_url: result.viewer_url,
    post_url: result.post_url,
  };
}

async function authenticatedJson(origin, endpoint, init = {}) {
  const response = await fetch(`${origin}${endpoint}`, { ...init, headers: { authorization: `Bearer ${token}`, ...(init.headers ?? {}) } });
  const body = await response.text();
  if (!response.ok) throw Object.assign(new Error(`daemon request failed with ${response.status}`), { code: "daemon_http", status: response.status });
  return body ? JSON.parse(body) : null;
}

async function startDaemon(candidate, origin, port, candidateBin) {
  await writeFile(tokenPath, token);
  await chmod(tokenPath, 0o600);
  await writeFile(configPath, JSON.stringify({
    schema_version: 1,
    store_root: store,
    bind: `127.0.0.1:${port}`,
    access: { mode: "token", token_file: tokenPath, public_origin: origin },
    limits: { max_upload_bytes: 16 * 1024 * 1024, max_finalized_blob_bytes: 128 * 1024 * 1024 },
  }));
  const env = daemonEnvironment(candidateBin);
  daemon = spawn(candidate, ["daemon"], { cwd: workspace, env, stdio: ["ignore", "ignore", "pipe"] });
  let stderr = "";
  daemon.stderr.on("data", (chunk) => {
    stderr += chunk;
    assertTokenAbsent(stderr, "daemon stderr");
    if (Buffer.byteLength(stderr) > limits.outputBytes) daemon.kill("SIGKILL");
  });
  for (let attempt = 0; attempt < 200; attempt += 1) {
    try {
      if ((await fetch(`${origin}/api/v1/health`)).ok) {
        env.GLIM_DAEMON_URL = origin;
        return env;
      }
    } catch {}
    if (attempt === 199) throw Object.assign(new Error("daemon health timeout"), { code: "daemon_start_timeout" });
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

async function chromiumPath() {
  for (const candidate of [process.env.CHROMIUM, "/snap/bin/chromium", "/usr/bin/chromium", "/usr/bin/chromium-browser", "/usr/bin/google-chrome"].filter(Boolean)) {
    try { await access(candidate); return candidate; } catch {}
  }
  throw Object.assign(new Error("Chromium executable unavailable"), { code: "chromium_missing" });
}

async function openBrowser(origin) {
  const debuggingPort = await reservePort();
  await mkdir(browserProfile, { recursive: true });
  browser = spawn(await chromiumPath(), [
    "--headless=new", "--no-sandbox", "--disable-gpu", `--remote-debugging-port=${debuggingPort}`,
    `--user-data-dir=${browserProfile}`, "about:blank",
  ], { stdio: ["ignore", "ignore", "pipe"] });
  let stderr = "";
  browser.stderr.on("data", (chunk) => { stderr += chunk; if (Buffer.byteLength(stderr) > limits.outputBytes) browser.kill("SIGKILL"); });
  let target;
  for (let attempt = 0; attempt < 200; attempt += 1) {
    try {
      const pages = await (await fetch(`http://127.0.0.1:${debuggingPort}/json/list`)).json();
      target = pages.find((page) => page.type === "page");
      if (target) break;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  if (!target) throw Object.assign(new Error("Chromium debugging timeout"), { code: "chromium_start_timeout" });
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.addEventListener("open", resolve, { once: true }); socket.addEventListener("error", reject, { once: true }); });
  let id = 0;
  const pending = new Map();
  const exceptions = [];
  socket.addEventListener("message", ({ data }) => {
    const message = JSON.parse(data);
    if (message.method === "Runtime.exceptionThrown") exceptions.push(message.params.exceptionDetails);
    const callback = pending.get(message.id);
    if (callback) { pending.delete(message.id); callback(message); }
  });
  const command = (method, params = {}) => new Promise((resolve, reject) => {
    const commandId = ++id;
    pending.set(commandId, (message) => message.error ? reject(new Error("CDP command failed")) : resolve(message.result));
    socket.send(JSON.stringify({ id: commandId, method, params }));
  });
  const evaluate = async (expression) => {
    const response = await command("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (response.exceptionDetails) throw new Error("browser evaluation failed");
    return response.result.value;
  };
  const waitFor = async (expression, label, attempts = 200) => {
    for (let attempt = 0; attempt < attempts; attempt += 1) {
      if (await evaluate(expression)) return;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    throw Object.assign(new Error(`browser wait failed: ${label}`), { code: "browser_wait" });
  };
  await command("Page.enable");
  await command("Runtime.enable");
  await command("Page.navigate", { url: `${origin}/feed` });
  await waitFor("location.pathname === '/login' && document.querySelector('glim-app')?.shadowRoot?.querySelector('input[type=password]')", "login page");
  await evaluate(`(() => { const r=document.querySelector('glim-app').shadowRoot; r.querySelector('input[type=password]').value=${JSON.stringify(token)}; r.querySelector('form').requestSubmit(); })()`);
  await waitFor("location.pathname === '/feed'", "login completion");
  return { socket, command, evaluate, waitFor, exceptions };
}

async function inspectBrowser(origin, projectId, primary, isolated) {
  const app = "document.querySelector('glim-app')?.shadowRoot";
  await cdp.command("Page.navigate", { url: `${origin}/projects/${projectId}` });
  await cdp.waitFor(`${app}?.querySelector('#post-${primary.post_id}') && ${app}?.querySelector('#post-${isolated.post_id}')`, "two-session project feed");
  const isolation = await cdp.evaluate(`(() => { const root=${app}; const posts=[...root.querySelectorAll('article[id^=post-]')]; return { primary: posts.filter(p=>p.id==='post-${primary.post_id}').length, isolated: posts.filter(p=>p.id==='post-${isolated.post_id}').length, sessions: [...new Set(posts.map(p=>p.querySelector('a[href^="/sessions/"]')?.getAttribute('href')))].filter(Boolean) }; })()`);
  assert.equal(isolation.primary, 1);
  assert.equal(isolation.isolated, 1);
  assert.equal(isolation.sessions.length, 2, "project feed leaked or merged session identity");
  await cdp.waitFor(`(() => { const post=${app}?.querySelector('#post-${primary.post_id}'); if(!post) return false; const artifacts=[...post.querySelectorAll('glim-artifact')]; return artifacts.length >= 9 && artifacts.every(a => a.shadowRoot && !a.shadowRoot.querySelector('.error')); })()`, "representative artifact renderers", 400);
  const state = await cdp.evaluate(`(() => { const post=${app}.querySelector('#post-${primary.post_id}'); return [...post.querySelectorAll('glim-artifact')].map(a => ({ renderer:a.data.file.renderer, filename:a.data.file.filename, html:a.shadowRoot.innerHTML, sandbox:a.shadowRoot.querySelector('iframe')?.getAttribute('sandbox') ?? null, media:a.shadowRoot.querySelector('audio,video')?.tagName ?? null })); })()`);
  const byRenderer = new Map(state.map((entry) => [entry.renderer, entry]));
  for (const renderer of rendererNames) assert(byRenderer.has(renderer), `browser lacks ${renderer}`);
  assert.match(byRenderer.get("markdown").html, /Collected support image/);
  assert.match(byRenderer.get("image").html, /preview-control/);
  assert.match(byRenderer.get("svg").html, /preview-control/);
  assert.match(byRenderer.get("text").html, /deterministic acceptance text/);
  assert.match(byRenderer.get("json").html, /accepted/);
  assert.match(byRenderer.get("csv").html, /<table/);
  assert.equal(byRenderer.get("html").sandbox, "");
  assert.match(byRenderer.get("html").html, /Safe HTML/);
  assert.match(byRenderer.get("pdf").html, /pdf-document/);
  assert.equal(byRenderer.get("audio").media, "AUDIO");
  assert.equal(byRenderer.get("video").media, "VIDEO");
  for (const renderer of rendererNames) renderers.set(renderer, "PASS");
}

async function assertRevisionLive(revision, predecessor) {
  const app = "document.querySelector('glim-app')?.shadowRoot";
  await cdp.waitFor(`${app}?.querySelector('#post-${revision.post_id}')`, "live revision insertion", 300);
  const order = await cdp.evaluate(`[...${app}.querySelectorAll('article[id^=post-]')].map(e=>Number(e.id.slice(5)))`);
  assert(order.indexOf(revision.post_id) < order.indexOf(predecessor), "revision was not inserted ahead of predecessor");
  const link = await cdp.evaluate(`${app}.querySelector('#post-${revision.post_id} a[href="#post-${predecessor}"]')?.textContent`);
  assert.match(link, new RegExp(`Revises post ${predecessor}`));
}

async function assertCommand(events, fragment) {
  const text = JSON.stringify(events);
  assert(text.includes(fragment), `Pi command output lacked ${fragment}`);
  assertTokenAbsent(text, "Pi command output");
}

function assertLiveMatrixPassed() {
  const blocked = matrix.filter(({ model, mode }) => !results.get(`${model}|${mode}`).startsWith("PASS"));
  if (blocked.length > 0) throw Object.assign(new Error("live Pi matrix did not pass"), { code: "live_model_matrix" });
}

async function closeAndVerify(origin, env, primary, isolated, codexRpc, haikuRpc) {
  if (codexRpc) {
    await assertCommand(await codexRpc.command("/glim-feed", "codex-feed", primary.public_session_id), primary.public_session_id);
    await assertCommand(await codexRpc.command("/glim-status", "codex-status", "active sessions"), "active sessions");
    await assertCommand(await codexRpc.command("/glim-close", "codex-close", "Closed Glimse session"), "Closed Glimse session");
  } else await cli(env, ["close", primary.public_session_id]);
  if (haikuRpc) {
    await assertCommand(await haikuRpc.command("/glim-feed", "haiku-feed", isolated.public_session_id), isolated.public_session_id);
    await assertCommand(await haikuRpc.command("/glim-status", "haiku-status", "active sessions"), "active sessions");
    await assertCommand(await haikuRpc.command("/glim-close", "haiku-close", "Closed Glimse session"), "Closed Glimse session");
  } else await cli(env, ["close", isolated.public_session_id]);
  for (const publicId of [primary.public_session_id, isolated.public_session_id]) {
    const response = await fetch(`${origin}/api/v1/sessions/${publicId}`, { headers: { authorization: `Bearer ${token}` } });
    assert.equal(response.status, 404, "closed session feed remained available");
  }
  const status = await cli(env, ["status"]);
  assert.equal(status.active_sessions, 0);
  assert.equal(status.finalized_unique_blob_bytes, 0);
  assert.equal(status.queued_blob_deletions, 0);
  closureResult = "PASS";
}

async function writeEvidence() {
  const lines = [
    "# Release acceptance record",
    "",
    "This record contains no credentials, public IDs, generated URLs, transcripts, or temporary paths.",
    "",
    "## Live Pi matrix",
    "",
    "| Model | Mode | Result |",
    "| --- | --- | --- |",
    ...matrix.map(({ model, mode }) => `| \`${model}\` | ${mode === "json" ? "JSON" : mode === "rpc" ? "RPC" : "print"} | ${results.get(`${model}|${mode}`)} |`),
    "",
    "Each passing publication row used an actual `glim_publish` model tool call with built-in tools disabled. RPC command checks covered `/glim-feed`, `/glim-status`, and `/glim-close` where the persisted model session was available. The harness permits one publication call and at most three model turns per path. Provider calls have a four-minute default deadline, combined subprocess output is capped at 2 MiB, and the complete run has a 30-minute default deadline.",
    "",
    "## Artifact and browser coverage",
    "",
    `Local install, package, daemon, and browser boundary: ${localResult}`,
    "",
    "| Renderer family | Result |",
    "| --- | --- |",
    ...rendererNames.map((renderer) => `| ${renderer} | ${renderers.get(renderer)} |`),
    "",
    `Session closure and purge: ${closureResult}`,
    "",
    "Fixtures covered Markdown with a collected image, SVG/image, text, JSON, CSV, HTML with collected CSS, PDF, audio, and video. Chromium used the token login form and left HTML scripts disabled. The browser check rejected runtime exceptions, required a live immutable revision before its predecessor, and required two exact Glimse session identities in one project feed.",
    "",
    "## Deferred release criteria",
    "",
    "A real release tag and downloaded GitHub release remain untested. A real user-service install also remains untested because acceptance must not alter user service state. The local archive reproduces the release archive layout and checksum procedure, but it does not replace those release criteria.",
    "",
  ];
  await writeFile(evidencePath, `${lines.join("\n")}\n`);
}

const totalDeadline = setTimeout(() => {
  fatal = Object.assign(new Error("release acceptance exceeded total deadline"), { code: "total_timeout", timedOut: true });
  daemon?.kill("SIGKILL");
  browser?.kill("SIGKILL");
}, limits.totalMs);

let codexRpc;
let haikuRpc;
try {
  await mkdir(sessions, { recursive: true });
  const { candidate, candidateBin } = await installCandidate();
  await packageFixture(candidate);
  await createFixtures();
  const port = await reservePort();
  const origin = `http://127.0.0.1:${port}`;
  const env = await startDaemon(candidate, origin, port, candidateBin);
  await cli(env, ["health"]);
  await cli(env, ["status"]);

  const codexSession = path.join(sessions, "codex.jsonl");
  const haikuSession = path.join(sessions, "haiku.jsonl");
  let primary;
  try {
    primary = await piJsonPublish(env, models.codex, codexSession, artifactNames);
    results.set(`${models.codex}|json`, "PASS (confirmed tool call)");
  } catch (error) {
    results.set(`${models.codex}|json`, `BLOCKED: ${sanitizedBlocker(error)}`);
    primary = await directPublish(env, "local-browser-primary", artifactNames);
  }
  let isolated;
  try {
    isolated = await piPrintPublish(env, models.haiku, haikuSession, ["diagram.svg"]);
    results.set(`${models.haiku}|print`, "PASS (confirmed tool call)");
  } catch (error) {
    results.set(`${models.haiku}|print`, `BLOCKED: ${sanitizedBlocker(error)}`);
    isolated = await directPublish(env, "local-browser-isolated", ["diagram.svg"]);
  }
  assert.notEqual(primary.public_session_id, isolated.public_session_id);

  const session = await authenticatedJson(origin, `/api/v1/sessions/${primary.public_session_id}`);
  cdp = await openBrowser(origin);
  await inspectBrowser(origin, session.project.id, primary, isolated);

  let revision;
  if (results.get(`${models.codex}|json`).startsWith("PASS")) {
    try {
      codexRpc = new RpcPi(env, models.codex, codexSession);
      await codexRpc.start();
      const events = await codexRpc.prompt(publicationPrompt(["notes.txt"], primary.post_id), "codex-revision");
      revision = confirmedPublication(events, `${models.codex} RPC`);
      assert.equal(revision.public_session_id, primary.public_session_id);
      assert.equal(revision.predecessor_post_id, primary.post_id);
      results.set(`${models.codex}|rpc`, "PASS (resumed revision and commands)");
    } catch (error) {
      results.set(`${models.codex}|rpc`, `BLOCKED: ${sanitizedBlocker(error)}`);
      await codexRpc?.close(); codexRpc = undefined;
      revision = await directPublish(env, "local-browser-primary", ["notes.txt"], primary.post_id);
    }
  } else {
    results.set(`${models.codex}|rpc`, "BLOCKED: prerequisite JSON provider path unavailable");
    revision = await directPublish(env, "local-browser-primary", ["notes.txt"], primary.post_id);
  }
  await assertRevisionLive(revision, primary.post_id);

  if (results.get(`${models.haiku}|print`).startsWith("PASS")) {
    try {
      haikuRpc = new RpcPi(env, models.haiku, haikuSession);
      await haikuRpc.start();
      await assertCommand(await haikuRpc.command("/glim-feed", "haiku-feed-before-close", isolated.public_session_id), isolated.public_session_id);
      await assertCommand(await haikuRpc.command("/glim-status", "haiku-status-before-close", "active sessions"), "active sessions");
      results.set(`${models.haiku}|rpc`, "PASS (resumed commands)");
    } catch (error) {
      results.set(`${models.haiku}|rpc`, `BLOCKED: ${sanitizedBlocker(error)}`);
      await haikuRpc?.close(); haikuRpc = undefined;
    }
  } else results.set(`${models.haiku}|rpc`, "BLOCKED: prerequisite print provider path unavailable");

  assert.equal(cdp.exceptions.length, 0, "Chromium reported runtime exceptions");
  localResult = "PASS";
  await closeAndVerify(origin, env, primary, isolated, codexRpc, haikuRpc);
  assertLiveMatrixPassed();
  console.log("Release acceptance completed; credential-free details are in docs/release-acceptance.md");
} catch (error) {
  fatal = fatal ?? error;
} finally {
  clearTimeout(totalDeadline);
  await codexRpc?.close().catch(() => undefined);
  await haikuRpc?.close().catch(() => undefined);
  cdp?.socket.close();
  await stop(browser).catch(() => undefined);
  await stop(daemon).catch(() => undefined);
  await writeEvidence();
  await rm(root, { recursive: true, force: true });
}

if (fatal) throw Object.assign(new Error(`release acceptance failed: ${sanitizedBlocker(fatal)}`), { cause: fatal });
