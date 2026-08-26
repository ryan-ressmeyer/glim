import { spawn } from "node:child_process";
import { access, mkdtemp, readFile, rm } from "node:fs/promises";
import http from "node:http";
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

const appScript = await readFile(new URL("../dist/assets/app.js", import.meta.url));
const session = {
  id: 7,
  public_id: "2zY8Ab",
  integration_namespace: "test",
  external_key: "chromium",
  project: { id: 42, label: "Chromium", working_directory: "/tmp/chromium" },
  created_at: 1,
  last_activity_at: 2,
};
const page = {
  posts: [{
    id: 1,
    session_id: 7,
    session_public_id: "2zY8Ab",
    title: "HTML opt-in",
    commentary: "Chromium regression",
    predecessor_post_id: null,
    published_at: 1,
    git: null,
    files: [{
      position: 0,
      filename: "entry.html",
      caption: null,
      media_type: "text/html; charset=utf-8",
      renderer: "html",
      support_assets: [{ relative_path: "app.js" }],
    }],
  }],
  next_cursor: null,
};
const entry = "<!doctype html><img src=\"/leak\"><iframe src=\"/leak\"></iframe><script>parent.postMessage('inline-ran', '*')</script><script src=\"app.js\"></script>";
const harness = `<script>
  window.__htmlMessages = [];
  addEventListener("message", (event) => window.__htmlMessages.push(event.data));
</script>`;
let supportRequests = 0;
let leakRequests = 0;

const server = http.createServer((request, response) => {
  const path = new URL(request.url, "http://127.0.0.1").pathname;
  if (path === "/feed" || path === "/") {
    response.setHeader("content-type", "text/html; charset=utf-8");
    response.end(`<!doctype html><html><head>${harness}</head><body><glim-app></glim-app><script type="module" src="/assets/app.js"></script></body></html>`);
  } else if (path === "/assets/app.js") {
    response.setHeader("content-type", "text/javascript; charset=utf-8");
    response.end(appScript);
  } else if (path === "/api/v1/posts") {
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify(page));
  } else if (path === "/api/v1/sessions/2zY8Ab") {
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify(session));
  } else if (path === "/api/v1/posts/1/files/0/content") {
    response.setHeader("content-type", "text/html; charset=utf-8");
    response.end(entry);
  } else if (path === "/api/v1/posts/1/files/0/html-capability") {
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ path_prefix: "/api/v1/posts/1/files/0/support/", expires_in_seconds: 300 }));
  } else if (path === "/api/v1/posts/1/files/0/support/app.js") {
    supportRequests += 1;
    response.setHeader("content-type", "text/javascript; charset=utf-8");
    response.end(`(async () => {
      let parentAccess = true;
      try { void parent.document.body; } catch { parentAccess = false; }
      let fetchBlocked = false;
      try { await fetch('/leak'); } catch { fetchBlocked = true; }
      parent.postMessage({ tag: 'support-ran', parentAccess, fetchBlocked, origin: location.origin }, '*');
    })()`);
  } else if (path === "/leak") {
    leakRequests += 1;
    response.statusCode = 204;
    response.end();
  } else {
    response.statusCode = 404;
    response.end();
  }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const address = server.address();
if (!address || typeof address === "string") throw new Error("test server did not bind a TCP port");

const portProbe = http.createServer();
await new Promise((resolve) => portProbe.listen(0, "127.0.0.1", resolve));
const probeAddress = portProbe.address();
if (!probeAddress || typeof probeAddress === "string") throw new Error("could not allocate a Chromium debugging port");
const debuggingPort = probeAddress.port;
await new Promise((resolve, reject) => portProbe.close((error) => error ? reject(error) : resolve()));
const profile = await mkdtemp(path.join(os.tmpdir(), "glim-chromium-"));
const browser = spawn(chromium, [
  "--headless=new",
  "--no-sandbox",
  "--disable-gpu",
  `--remote-debugging-port=${debuggingPort}`,
  `--user-data-dir=${profile}`,
  "about:blank",
], { stdio: ["ignore", "ignore", "pipe"] });
let browserErrors = "";
browser.stderr.on("data", (chunk) => { browserErrors += chunk; });

let socket;
try {
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
  socket.addEventListener("message", ({ data }) => {
    const message = JSON.parse(data);
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
    const response = await command("Runtime.evaluate", { expression, returnByValue: true });
    if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
    return response.result.value;
  };
  const waitFor = async (expression, label) => {
    for (let attempt = 0; attempt < 100; attempt += 1) {
      if (await evaluate(expression)) return;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    throw new Error(`timed out waiting for ${label}`);
  };

  await command("Page.enable");
  await command("Runtime.enable");
  await command("Page.navigate", { url: `http://127.0.0.1:${address.port}/feed` });
  const artifact = "document.querySelector('glim-app')?.shadowRoot?.querySelector('glim-artifact')";
  await waitFor(`${artifact}?.shadowRoot?.querySelector('[data-enable-scripts]') !== null`, "the HTML artifact");
  await new Promise((resolve) => setTimeout(resolve, 200));
  const safeMessages = await evaluate("window.__htmlMessages");
  const safeState = await evaluate(`(() => {
    const frame = ${artifact}.shadowRoot.querySelector('iframe');
    let opaque = false;
    try { void frame.contentWindow.document.body; } catch { opaque = true; }
    return { sandbox: frame.getAttribute('sandbox'), opaque };
  })()`);
  if (safeMessages.length !== 0 || safeState.sandbox !== "" || !safeState.opaque || leakRequests !== 0) {
    throw new Error(`safe mode was not inert and opaque: state=${JSON.stringify(safeState)}, messages=${JSON.stringify(safeMessages)}, leaks=${leakRequests}`);
  }

  await evaluate(`${artifact}.shadowRoot.querySelector('[data-enable-scripts]').click()`);
  await waitFor("window.__htmlMessages.includes('inline-ran') && window.__htmlMessages.some((value) => value?.tag === 'support-ran')", "opt-in script execution");
  const optIn = await evaluate(`(() => { const frame=${artifact}.shadowRoot.querySelector('iframe'); return { sandbox: frame.getAttribute('sandbox'), srcdoc: frame.srcdoc, messages: window.__htmlMessages }; })()`);
  if (optIn.sandbox !== "allow-scripts") throw new Error(`unexpected opt-in sandbox: ${JSON.stringify(optIn)}`);
  const supportResult = optIn.messages.find((value) => value?.tag === "support-ran");
  if (!supportResult || supportResult.parentAccess || !supportResult.fetchBlocked || supportResult.origin !== "null") {
    throw new Error(`script isolation failed: ${JSON.stringify(optIn)}`);
  }
  if (supportRequests !== 1 || leakRequests !== 0) {
    throw new Error(`expected one support-script request and no network leak; support=${supportRequests}, leaks=${leakRequests}\n${optIn.srcdoc}`);
  }
  console.log("Chromium HTML opt-in: safe mode inert and opaque; opt-in scripts isolated with fetch blocked");
} catch (error) {
  throw new Error(`${error.message}\nChromium stderr:\n${browserErrors}`);
} finally {
  socket?.close();
  browser.kill("SIGTERM");
  await rm(profile, { recursive: true, force: true });
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
}
