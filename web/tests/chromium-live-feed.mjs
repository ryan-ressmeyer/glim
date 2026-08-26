import { spawn } from "node:child_process";
import { access, chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";

const chromiumCandidates = [
  process.env.CHROMIUM,
  "/snap/bin/chromium",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
  "/usr/bin/google-chrome",
].filter(Boolean);
let chromium;
for (const candidate of chromiumCandidates) {
  try {
    await access(candidate);
    chromium = candidate;
    break;
  } catch {
    // Try the next supported executable location.
  }
}
if (!chromium) throw new Error("Chromium executable not found; set CHROMIUM to run this regression");

const daemonOrigin = "http://127.0.0.1:3030";
const daemonBinary = new URL("../../target/debug/glim", import.meta.url).pathname;
await access(daemonBinary);
const storeRoot = await mkdtemp(path.join(os.tmpdir(), "glim-live-store-"));
const profile = await mkdtemp(path.join(os.tmpdir(), "glim-live-chromium-"));
const accessToken = "b".repeat(64);
const tokenPath = path.join(storeRoot, "access-token");
const configPath = path.join(storeRoot, "config.json");
await writeFile(tokenPath, accessToken);
await chmod(tokenPath, 0o600);
await writeFile(configPath, JSON.stringify({
  schema_version: 1,
  store_root: path.join(storeRoot, "store"),
  bind: "127.0.0.1:3030",
  access: {
    mode: "token",
    token_file: tokenPath,
    public_origin: "http://127.0.0.1:3030",
  },
}));

const portAvailable = await new Promise((resolve) => {
  const probe = net.createServer();
  probe.once("error", () => resolve(false));
  probe.listen(3030, "127.0.0.1", () => probe.close(() => resolve(true)));
});
if (!portAvailable) throw new Error("Chromium live-feed regression requires free port 3030");

const daemonEnvironment = { ...process.env, GLIM_CONFIG: configPath };
for (const name of [
  "GLIM_STORE_ROOT", "GLIM_BIND", "GLIM_ACCESS_MODE", "GLIM_TOKEN_FILE",
  "GLIM_PUBLIC_ORIGIN", "GLIM_TLS_CERTIFICATE", "GLIM_TLS_PRIVATE_KEY",
  "GLIM_TRUSTED_PROXY_IPS",
]) delete daemonEnvironment[name];
const daemon = spawn(daemonBinary, ["daemon"], {
  env: daemonEnvironment,
  stdio: ["ignore", "ignore", "pipe"],
});
let daemonErrors = "";
daemon.stderr.on("data", (chunk) => { daemonErrors += chunk; });

const authenticatedHeaders = (contentType) => ({
  authorization: `Bearer ${accessToken}`,
  ...(contentType ? { "content-type": contentType } : {}),
});

const publish = async (externalKey, title) => {
  const boundary = `chromium-${externalKey}-${Date.now()}-${Math.random()}`;
  const manifest = JSON.stringify({
    integration_namespace: "chromium",
    external_key: externalKey,
    project_label: "Live project",
    working_directory: "/tmp/glim-live-project",
    title,
    commentary: `Commentary for ${title}`,
    files: [{ part: "file", filename: `${title}.txt`, support_assets: [] }],
  });
  const body = [
    `--${boundary}\r\nContent-Disposition: form-data; name="manifest"\r\n\r\n${manifest}\r\n`,
    `--${boundary}\r\nContent-Disposition: form-data; name="file"\r\n\r\n${title}\r\n`,
    `--${boundary}--\r\n`,
  ].join("");
  const response = await fetch(`${daemonOrigin}/api/v1/posts`, {
    method: "POST",
    headers: authenticatedHeaders(`multipart/form-data; boundary=${boundary}`),
    body,
  });
  if (response.status !== 201) throw new Error(`publish ${title} failed: ${response.status} ${await response.text()}`);
  return response.json();
};

const publishHtml = async (externalKey) => {
  const boundary = `chromium-html-${Date.now()}`;
  const manifest = JSON.stringify({
    integration_namespace: "chromium",
    external_key: externalKey,
    project_label: "Live project",
    working_directory: "/tmp/glim-live-project",
    title: "Authenticated HTML",
    commentary: "Capability-backed support script",
    files: [{
      part: "entry",
      filename: "authenticated.html",
      support_assets: [{ part: "script", relative_path: "app.js" }],
    }],
  });
  const body = [
    `--${boundary}\r\nContent-Disposition: form-data; name="manifest"\r\n\r\n${manifest}\r\n`,
    `--${boundary}\r\nContent-Disposition: form-data; name="entry"\r\n\r\n<script src="app.js"></script>\r\n`,
    `--${boundary}\r\nContent-Disposition: form-data; name="script"\r\n\r\nparent.postMessage('authenticated-support-ran', '*')\r\n`,
    `--${boundary}--\r\n`,
  ].join("");
  const response = await fetch(`${daemonOrigin}/api/v1/posts`, {
    method: "POST",
    headers: authenticatedHeaders(`multipart/form-data; boundary=${boundary}`),
    body,
  });
  if (response.status !== 201) throw new Error(`HTML publish failed: ${response.status} ${await response.text()}`);
  return response.json();
};

let browser;
let socket;
let browserErrors = "";
try {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      if ((await fetch(`${daemonOrigin}/api/v1/health`)).ok) break;
    } catch {
      // The daemon has not bound yet.
    }
    if (attempt === 99) throw new Error(`daemon did not start\n${daemonErrors}`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }

  const firstA = await publish("session-a", "A initial");
  const firstB = await publish("session-b", "B initial");
  const authenticatedHtml = await publishHtml("session-a");
  const sessionA = firstA.session.public_id;
  const sessionB = firstB.session.public_id;
  const projectId = firstA.session.project.id;
  if (firstB.session.project.id !== projectId) throw new Error("fixtures did not resolve to one project");

  const portProbe = net.createServer();
  await new Promise((resolve) => portProbe.listen(0, "127.0.0.1", resolve));
  const probeAddress = portProbe.address();
  if (!probeAddress || typeof probeAddress === "string") throw new Error("could not allocate Chromium debugging port");
  const debuggingPort = probeAddress.port;
  await new Promise((resolve, reject) => portProbe.close((error) => error ? reject(error) : resolve()));

  browser = spawn(chromium, [
    "--headless=new",
    "--no-sandbox",
    "--disable-gpu",
    `--remote-debugging-port=${debuggingPort}`,
    `--user-data-dir=${profile}`,
    "about:blank",
  ], { stdio: ["ignore", "ignore", "pipe"] });
  browser.stderr.on("data", (chunk) => { browserErrors += chunk; });

  let version;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const response = await fetch(`http://127.0.0.1:${debuggingPort}/json/version`);
      if (response.ok) {
        version = await response.json();
        break;
      }
    } catch {
      // Chromium has not opened its debugging socket yet.
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  if (!version) throw new Error(`Chromium debugging endpoint did not start\n${browserErrors}`);
  const pages = await (await fetch(`http://127.0.0.1:${debuggingPort}/json/list`)).json();
  const pageTarget = pages.find((candidate) => candidate.type === "page");
  if (!pageTarget) throw new Error("Chromium did not expose a page target");

  socket = new WebSocket(pageTarget.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", reject, { once: true });
  });
  let commandId = 0;
  const pending = new Map();
  const runtimeExceptions = [];
  socket.addEventListener("message", ({ data }) => {
    const message = JSON.parse(data);
    if (message.method === "Runtime.exceptionThrown") runtimeExceptions.push(message.params.exceptionDetails);
    if (!message.id) return;
    const callback = pending.get(message.id);
    if (callback) {
      pending.delete(message.id);
      callback(message);
    }
  });
  const command = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++commandId;
    pending.set(id, (message) => message.error ? reject(new Error(JSON.stringify(message.error))) : resolve(message.result));
    socket.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async (expression) => {
    const response = await command("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
    return response.result.value;
  };
  const waitFor = async (expression, label, attempts = 120) => {
    for (let attempt = 0; attempt < attempts; attempt += 1) {
      if (await evaluate(expression)) return;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    throw new Error(`timed out waiting for ${label}`);
  };
  const app = "document.querySelector('glim-app')?.shadowRoot";

  await command("Page.enable");
  await command("Runtime.enable");
  await command("Page.navigate", { url: `${daemonOrigin}/projects/${projectId}` });
  await waitFor("location.pathname === '/login' && document.querySelector('glim-app')?.shadowRoot?.querySelector('input[type=password]')", "token login redirect");
  await evaluate(`(() => {
    const root = document.querySelector('glim-app').shadowRoot;
    root.querySelector('input[type=password]').value = '${accessToken}';
    root.querySelector('form').requestSubmit();
  })()`);
  await waitFor("location.pathname === '/feed'", "browser session login");
  await command("Page.navigate", { url: `${daemonOrigin}/projects/${projectId}` });
  await waitFor(`${app}?.querySelector('#post-${firstA.post.id}') && ${app}?.querySelector('#post-${firstB.post.id}') && ${app}?.querySelector('#post-${authenticatedHtml.post.id}')`, "initial two-session project feed");
  await evaluate(`(() => {
    window.__authenticatedMessages = [];
    addEventListener('message', (event) => window.__authenticatedMessages.push(event.data));
    document.querySelector('glim-app').shadowRoot.querySelector('#post-${firstB.post.id}').dataset.preserved = 'yes';
  })()`);
  const authenticatedArtifact = `${app}.querySelector('#post-${authenticatedHtml.post.id} glim-artifact')?.shadowRoot`;
  await waitFor(`${authenticatedArtifact}?.querySelector('[data-enable-scripts]')`, "authenticated HTML artifact");
  const capabilitySource = await evaluate(`${authenticatedArtifact}.querySelector('iframe').srcdoc`);
  if (!capabilitySource.includes("/cap/")) throw new Error("authenticated HTML did not receive a scoped capability");
  await evaluate(`${authenticatedArtifact}.querySelector('[data-enable-scripts]').click()`);
  await waitFor("window.__authenticatedMessages.includes('authenticated-support-ran')", "capability-backed support script");

  const liveA = await publish("session-a", "A live");
  await waitFor(`${app}?.querySelector('#post-${liveA.post.id}')`, "top-of-page live insertion");
  const preserved = await evaluate(`${app}.querySelector('#post-${firstB.post.id}')?.dataset.preserved`);
  if (preserved !== "yes") throw new Error("live insertion recreated an existing renderer node");

  await evaluate("document.body.style.minHeight='4000px'; window.scrollTo(0, 600)");
  await waitFor("window.scrollY > 8", "a scrolled viewport");
  const queuedB = await publish("session-b", "B queued");
  await waitFor(`${app}?.querySelector('[data-new-posts]') && !${app}?.querySelector('#post-${queuedB.post.id}')`, "viewport-preserving live queue");
  await evaluate(`${app}.querySelector('[data-new-posts]').click()`);
  await waitFor(`${app}?.querySelector('#post-${queuedB.post.id}')`, "queued post activation");

  const closedA = await fetch(`${daemonOrigin}/api/v1/sessions/${sessionA}`, {
    method: "DELETE",
    headers: { authorization: `Bearer ${accessToken}` },
  });
  if (!closedA.ok) throw new Error(`session A close failed: ${closedA.status}`);
  await waitFor(`!${app}?.querySelector('#post-${firstA.post.id}') && !${app}?.querySelector('#post-${liveA.post.id}') && ${app}?.querySelector('#post-${firstB.post.id}') && ${app}?.querySelector('#post-${queuedB.post.id}')`, "cross-session closure reconciliation");

  const beforeHeartbeat = await (await fetch(`${daemonOrigin}/api/v1/sessions/${sessionB}`, {
    headers: { authorization: `Bearer ${accessToken}` },
  })).json();
  await new Promise((resolve) => setTimeout(resolve, 1100));
  await command("Page.navigate", { url: `${daemonOrigin}/sessions/${sessionB}` });
  await waitFor(`${app}?.querySelector('#post-${queuedB.post.id}')`, "session feed");
  await waitFor(`fetch('/api/v1/sessions/${sessionB}').then((response) => response.json()).then((value) => value.last_activity_at > ${beforeHeartbeat.last_activity_at})`, "visible-session heartbeat");

  await evaluate("window.confirm = () => true");
  await evaluate(`${app}.querySelector('[data-close-session]').click()`);
  await waitFor(`${app}?.querySelector('.state')?.textContent === 'Session closed'`, "confirmed browser close");
  await evaluate(`${app}.querySelector('[data-logout]').click()`);
  await waitFor("location.pathname === '/login'", "browser logout");
  const postLogoutStatus = await evaluate("fetch('/api/v1/posts').then((response) => response.status)");
  if (postLogoutStatus !== 401) throw new Error(`logout retained API access: ${postLogoutStatus}`);

  if (runtimeExceptions.length > 0) throw new Error(`browser runtime exceptions: ${JSON.stringify(runtimeExceptions)}`);
  console.log("Chromium live feed: insertion, queueing, closure, heartbeat, and confirmed close passed across two sessions");
} catch (error) {
  throw new Error(`${error.message}\nDaemon stderr:\n${daemonErrors}\nChromium stderr:\n${browserErrors}`);
} finally {
  socket?.close();
  browser?.kill("SIGTERM");
  daemon.kill("SIGTERM");
  await rm(profile, { recursive: true, force: true });
  await rm(storeRoot, { recursive: true, force: true });
}
