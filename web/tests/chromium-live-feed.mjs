import { spawn } from "node:child_process";
import { access, chmod, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
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

const reservePort = async () => {
  const probe = net.createServer();
  await new Promise((resolve, reject) => probe.listen(0, "127.0.0.1", resolve).once("error", reject));
  const address = probe.address();
  if (!address || typeof address === "string") throw new Error("could not allocate a daemon port");
  await new Promise((resolve, reject) => probe.close((error) => error ? reject(error) : resolve()));
  return address.port;
};

const daemonPort = await reservePort();
const daemonOrigin = `http://127.0.0.1:${daemonPort}`;
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
  bind: `127.0.0.1:${daemonPort}`,
  access: {
    mode: "token",
    token_file: tokenPath,
    public_origin: daemonOrigin,
  },
}));

const daemonEnvironment = { ...process.env, GLIM_CONFIG: configPath };
for (const name of [
  "GLIM_STORE_ROOT", "GLIM_BIND", "GLIM_ACCESS_MODE", "GLIM_TOKEN_FILE",
  "GLIM_PUBLIC_ORIGIN", "GLIM_TLS_CERTIFICATE", "GLIM_TLS_PRIVATE_KEY",
  "GLIM_TRUSTED_PROXY_IPS", "GLIM_MAX_UPLOAD_BYTES",
  "GLIM_MAX_FINALIZED_BLOB_BYTES", "GLIM_MAX_STAGING_BYTES", "GLIM_MAX_CONCURRENT_PUBLICATIONS", "GLIM_LOG_LEVEL",
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

const largePdfBytes = (pageCount) => {
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    `<< /Type /Pages /Kids [${Array.from({ length: pageCount }, (_, index) => `${4 + index * 2} 0 R`).join(" ")}] /Count ${pageCount} >>`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
  ];
  for (let page = 1; page <= pageCount; page += 1) {
    const content = `BT /F1 18 Tf 72 720 Td (Large PDF page ${page}) Tj ET\n%${"x".repeat(32_768)}\n`;
    const contentId = 5 + (page - 1) * 2;
    objects.push(`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents ${contentId} 0 R >>`);
    objects.push(`<< /Length ${Buffer.byteLength(content)} >>\nstream\n${content}endstream`);
  }
  const chunks = [Buffer.from("%PDF-1.7\n%Glim\n")];
  const offsets = [0];
  let length = chunks[0].length;
  objects.forEach((object, index) => {
    offsets[index + 1] = length;
    const chunk = Buffer.from(`${index + 1} 0 obj\n${object}\nendobj\n`);
    chunks.push(chunk);
    length += chunk.length;
  });
  const xrefOffset = length;
  chunks.push(Buffer.from(`xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets.slice(1).map((offset) => `${String(offset).padStart(10, "0")} 00000 n `).join("\n")}\ntrailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xrefOffset}\n%%EOF\n`));
  return Buffer.concat(chunks);
};

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

const publishPdf = async (externalKey, bytes) => {
  const form = new FormData();
  form.append("manifest", JSON.stringify({
    integration_namespace: "chromium",
    external_key: externalKey,
    project_label: "Live project",
    working_directory: "/tmp/glim-live-project",
    title: "Large PDF",
    commentary: "Browser-native PDF regression",
    files: [{ part: "file", filename: "large-document.pdf", support_assets: [] }],
  }));
  form.append("file", new Blob([bytes], { type: "application/pdf" }), "large-document.pdf");
  const response = await fetch(`${daemonOrigin}/api/v1/posts`, {
    method: "POST",
    headers: authenticatedHeaders(),
    body: form,
  });
  if (response.status !== 201) throw new Error(`PDF publish failed: ${response.status} ${await response.text()}`);
  return response.json();
};

const publishHtml = async (externalKey, title, supportScript) => {
  const boundary = `chromium-html-${Date.now()}-${Math.random()}`;
  const manifest = JSON.stringify({
    integration_namespace: "chromium",
    external_key: externalKey,
    project_label: "Live project",
    working_directory: "/tmp/glim-live-project",
    title,
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
    `--${boundary}\r\nContent-Disposition: form-data; name="script"\r\n\r\n${supportScript}\r\n`,
    `--${boundary}--\r\n`,
  ].join("");
  const response = await fetch(`${daemonOrigin}/api/v1/posts`, {
    method: "POST",
    headers: authenticatedHeaders(`multipart/form-data; boundary=${boundary}`),
    body,
  });
  if (response.status !== 201) throw new Error(`HTML publish ${title} failed: ${response.status} ${await response.text()}`);
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
  const pdfBytes = largePdfBytes(200);
  if (pdfBytes.length < 5 * 1024 * 1024) throw new Error(`large PDF fixture is only ${pdfBytes.length} bytes`);
  const largePdf = await publishPdf("session-pdf", pdfBytes);
  const victimHtml = await publishHtml(
    "session-b",
    "Boundary victim",
    "parent.postMessage('cross-boundary-script-ran', '*')",
  );
  const authenticatedHtml = await publishHtml(
    "session-a",
    "Authenticated HTML",
    `parent.postMessage('authenticated-support-ran', '*');
const target = document.currentScript.src.replace(/\\/posts\\/\\d+\\//, '/posts/${victimHtml.post.id}/');
parent.postMessage({ tag: 'cross-boundary-attempted', url: target }, '*');
fetch(target, { credentials: 'omit' }).then((response) => response.text()).then((body) => parent.postMessage({ tag: 'cross-boundary-bytes', body }, '*')).catch(() => {});
const injected = document.createElement('script'); injected.src = target; document.head.append(injected);`,
  );
  const sessionA = firstA.session.public_id;
  const sessionB = firstB.session.public_id;
  const pdfSession = largePdf.session.public_id;
  const projectId = firstA.session.project.id;
  if (firstB.session.project.id !== projectId || largePdf.session.project.id !== projectId) throw new Error("fixtures did not resolve to one project");
  const pdfContentPath = `/api/v1/posts/${largePdf.post.id}/files/0/content`;
  const pdfHeaders = await fetch(`${daemonOrigin}${pdfContentPath}`, {
    method: "HEAD",
    headers: authenticatedHeaders(),
  });
  if (pdfHeaders.headers.get("content-type") !== "application/pdf") throw new Error("large PDF response lost its media type");
  if (pdfHeaders.headers.get("accept-ranges") !== "bytes") throw new Error("large PDF response lost range support");
  if (pdfHeaders.headers.get("content-security-policy") !== "default-src 'none'; sandbox") throw new Error("large PDF response lost its artifact sandbox");

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
  await command("Page.navigate", { url: `${daemonOrigin}/sessions/${pdfSession}` });
  const pdfArtifact = `${app}?.querySelector('#post-${largePdf.post.id} glim-artifact')?.shadowRoot`;
  await waitFor(`${pdfArtifact}?.querySelector('iframe.pdf-frame')`, "large native PDF frame");
  const pdfState = await evaluate(`(() => {
    const root = ${pdfArtifact};
    const frame = root.querySelector('iframe.pdf-frame');
    return {
      src: frame.getAttribute('src'),
      loading: frame.loading,
      title: frame.title,
      height: frame.getBoundingClientRect().height,
      viewportHeight: innerHeight,
      frameCount: root.querySelectorAll('iframe').length,
      canvasCount: root.querySelectorAll('canvas').length,
      open: root.querySelector('a[target="_blank"]')?.textContent,
      download: root.querySelector('a[download]')?.getAttribute('download'),
    };
  })()`);
  if (pdfState.src !== pdfContentPath || pdfState.loading !== "lazy" || pdfState.title !== "PDF: large-document.pdf") throw new Error(`native PDF frame contract failed: ${JSON.stringify(pdfState)}`);
  if (pdfState.frameCount !== 1 || pdfState.canvasCount !== 0) throw new Error(`large PDF expanded into renderer-owned pages: ${JSON.stringify(pdfState)}`);
  if (Math.abs(pdfState.height - pdfState.viewportHeight * 0.7) > 2) throw new Error(`large PDF frame is not bounded to 70vh: ${JSON.stringify(pdfState)}`);
  if (pdfState.open !== "Open" || pdfState.download !== "large-document.pdf") throw new Error(`large PDF fallbacks are missing: ${JSON.stringify(pdfState)}`);
  let nativePdfFrame;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const frameTree = (await command("Page.getFrameTree")).frameTree;
    const pendingFrames = [frameTree];
    while (pendingFrames.length > 0) {
      const candidate = pendingFrames.pop();
      if (candidate.frame.url.endsWith(pdfContentPath) && candidate.frame.mimeType === "application/pdf") nativePdfFrame = candidate.frame;
      pendingFrames.push(...(candidate.childFrames ?? []));
    }
    if (nativePdfFrame) break;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  if (!nativePdfFrame) throw new Error("Chromium did not load the sandboxed artifact as a native application/pdf frame");

  await command("Page.navigate", { url: `${daemonOrigin}/projects/${projectId}` });
  await waitFor(`${app}?.querySelector('#post-${firstA.post.id}') && ${app}?.querySelector('#post-${firstB.post.id}') && ${app}?.querySelector('#post-${victimHtml.post.id}') && ${app}?.querySelector('#post-${authenticatedHtml.post.id}')`, "initial two-session project feed");
  await evaluate(`(() => {
    window.__authenticatedMessages = [];
    addEventListener('message', (event) => window.__authenticatedMessages.push(event.data));
    document.querySelector('glim-app').shadowRoot.querySelector('#post-${firstB.post.id}').dataset.preserved = 'yes';
  })()`);
  const authenticatedArtifact = `${app}.querySelector('#post-${authenticatedHtml.post.id} glim-artifact')?.shadowRoot`;
  await waitFor(`${authenticatedArtifact}?.querySelector('[data-enable-scripts]')`, "authenticated HTML artifact");
  const capabilitySource = await evaluate(`${authenticatedArtifact}.querySelector('iframe').srcdoc`);
  if (!capabilitySource.includes("/cap/")) throw new Error("authenticated HTML did not receive a scoped capability script URL");
  await evaluate(`${authenticatedArtifact}.querySelector('[data-enable-scripts]').click()`);
  await waitFor("window.__authenticatedMessages.includes('authenticated-support-ran')", "capability-backed support script");
  await waitFor(`window.__authenticatedMessages.some((value) => value?.tag === 'cross-boundary-attempted' && value.url.includes('/posts/${victimHtml.post.id}/files/0/support/app.js'))`, "browser-derived cross-subtree capability URL");
  const attemptedUrl = await evaluate("window.__authenticatedMessages.find((value) => value?.tag === 'cross-boundary-attempted').url");
  const optedInCapabilitySource = await evaluate(`${authenticatedArtifact}.querySelector('iframe').srcdoc`);
  const currentCapabilityPath = optedInCapabilitySource.match(/src="(\/cap\/[^\"]+\/api\/v1\/posts\/\d+\/files\/0\/support\/app\.js)"/)?.[1];
  if (!currentCapabilityPath) throw new Error("opted-in HTML did not retain a scoped capability script URL");
  const currentCapabilityUrl = new URL(currentCapabilityPath, daemonOrigin);
  const attempted = new URL(attemptedUrl);
  if (attempted.href === currentCapabilityUrl.href) throw new Error("cross-subtree attempt retained the authorized URL");
  if (!attempted.pathname.includes(`/posts/${victimHtml.post.id}/files/0/support/app.js`)) throw new Error(`attempt did not target victim subtree: ${attempted.href}`);
  if (attempted.pathname.match(/^\/cap\/([^/]+)/)?.[1] !== currentCapabilityUrl.pathname.match(/^\/cap\/([^/]+)/)?.[1]) {
    throw new Error(`cross-subtree attempt did not reuse the current artifact capability: current=${currentCapabilityUrl.href} attempted=${attempted.href}`);
  }
  const daemonBoundaryResponse = await fetch(attempted, { credentials: "omit", redirect: "manual" });
  const daemonBoundaryBody = await daemonBoundaryResponse.text();
  let daemonBoundaryError;
  try { daemonBoundaryError = JSON.parse(daemonBoundaryBody); } catch {}
  if (daemonBoundaryResponse.status !== 404 || daemonBoundaryError?.error?.code !== "artifact_not_found") {
    throw new Error(`daemon did not reject foreign capability subtree: ${daemonBoundaryResponse.status} ${daemonBoundaryBody}`);
  }
  if (daemonBoundaryBody.includes("cross-boundary-script-ran")) throw new Error("daemon returned victim script bytes");
  await new Promise((resolve) => setTimeout(resolve, 250));
  const boundaryLeak = await evaluate("window.__authenticatedMessages.some((value) => value === 'cross-boundary-script-ran' || value?.tag === 'cross-boundary-bytes')");
  if (boundaryLeak) throw new Error("foreign capability subtree returned script or bytes to the attacker frame");

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
  await evaluate(`(() => { const root = ${app}; root.querySelector('[data-actions]').open = true; root.querySelector('[data-close-session]').click(); })()`);
  await waitFor(`${app}?.querySelector('.state')?.textContent === 'Session closed'`, "confirmed browser close");
  await evaluate(`(() => { const root = ${app}; root.querySelector('[data-actions]').open = true; root.querySelector('[data-logout]').click(); })()`);
  await waitFor("location.pathname === '/login'", "browser logout");
  const postLogoutStatus = await evaluate("fetch('/api/v1/posts').then((response) => response.status)");
  if (postLogoutStatus !== 401) throw new Error(`logout retained API access: ${postLogoutStatus}`);

  // Use another isolated session for inspection and browser-work budgets.
  const publishInspection = async (title, files, predecessor = null) => {
    const form = new FormData();
    form.append("manifest", JSON.stringify({
      integration_namespace: "pi", external_key: "inspection", project_label: "Visual neuroscience",
      working_directory: "/tmp/glim-inspection/population-response", title,
      commentary: "Compare the response curves and inspect the fit summary.\n\nBlue: control. Orange: adapted. These are illustrative test fixtures.",
      predecessor_post_id: predecessor,
      files: files.map((file, index) => ({ part: `file${index}`, filename: file.name, caption: file.caption ?? null, support_assets: [] })),
    }));
    files.forEach((file, index) => form.append(`file${index}`, new Blob([file.bytes]), file.name));
    const response = await fetch(`${daemonOrigin}/api/v1/posts`, { method: "POST", headers: authenticatedHeaders(), body: form });
    if (response.status !== 201) throw new Error(`inspection fixture failed: ${response.status} ${await response.text()}`);
    return response.json();
  };
  const tableBytes = JSON.stringify({ values: Array.from({ length: 12_000 }, (_, index) => index) });
  let inspection;
  for (let index = 0; index < 30; index += 1) {
    inspection = await publishInspection(`Fit summary ${index}`, [{ name: `fit-${index}.json`, bytes: tableBytes }]);
  }
  await command("Emulation.setDeviceMetricsOverride", { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
  await command("Page.navigate", { url: `${daemonOrigin}/login` });
  await waitFor(`${app}?.querySelector('input[type=password]')`, "inspection login");
  await evaluate(`(() => { const root=${app}; root.querySelector('input[type=password]').value='${accessToken}'; root.querySelector('form').requestSubmit(); })()`);
  await waitFor("location.pathname === '/feed'", "inspection login complete");
  await command("Page.navigate", { url: `${daemonOrigin}/sessions/${inspection.session.public_id}` });
  await waitFor(`${app}?.querySelectorAll('article').length >= 20`, "long inspection feed");
  await waitFor(`${app}?.querySelector('glim-artifact')?.shadowRoot?.querySelector('pre')`, "visible document render");
  await new Promise((resolve) => setTimeout(resolve, 500));
  const loading = await evaluate(`(() => {
    const artifacts=Array.from(${app}.querySelectorAll('glim-artifact'));
    const requests=performance.getEntriesByType('resource').filter((entry)=>new URL(entry.name).pathname.endsWith('/files/0/content'));
    return { artifacts:artifacts.length, rendered:artifacts.filter((artifact)=>artifact.shadowRoot.querySelector('pre')).length, downloaded:requests.length, decoded_bytes:requests.reduce((total,entry)=>total+entry.decodedBodySize,0) };
  })()`);
  if (loading.downloaded > 6 || loading.rendered > 6 || loading.downloaded === 0) {
    throw new Error(`offscreen rendering budget exceeded: ${JSON.stringify(loading)}`);
  }

  const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="420" viewBox="0 0 1000 420"><rect width="1000" height="420" fill="#f8fafc"/><text x="70" y="45" font-family="sans-serif" font-size="24" fill="#182a45">Population response across contrast levels</text><path d="M90 85V345H935" fill="none" stroke="#94a3b8" stroke-width="2"/><path d="M95 331L260 294L425 225L590 151L755 113L920 102" fill="none" stroke="#477be3" stroke-width="5"/><path d="M95 335L260 318L425 281L590 225L755 188L920 174" fill="none" stroke="#e38c48" stroke-width="5"/></svg>';
  const firstFigure = await publishInspection("Contrast response: initial fit", [{ name: "initial-response.svg", bytes: svg }]);
  const revision = await publishInspection("Contrast response: revised fit", [
    { name: "population-response.svg", caption: "Mean response by contrast. Illustrative fixture, not experimental results.", bytes: svg },
    { name: "fit-summary.json", bytes: JSON.stringify({ model: "Naka-Rushton", conditions: ["control", "adapted"], retained_trials: 184, excluded_trials: 16 }) },
  ], firstFigure.post.id);
  await command("Page.navigate", { url: `${daemonOrigin}/sessions/${inspection.session.public_id}` });
  const revisionRoot = `${app}.querySelector('#post-${revision.post.id} glim-artifact').shadowRoot`;
  await waitFor(`${app}?.querySelector('#post-${revision.post.id} glim-artifact')?.shadowRoot?.querySelector('img')?.complete`, "revision image");
  await waitFor(`${app}?.querySelector('nav')?.textContent.includes('Visual neuroscience')`, "project context");
  const screenshotDirectory = process.env.GLIM_INSPECTION_SCREENSHOTS;
  const screenshot = async (name) => {
    if (!screenshotDirectory) return;
    await mkdir(screenshotDirectory, { recursive: true });
    const capture = await command("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
    await writeFile(path.join(screenshotDirectory, name), Buffer.from(capture.data, "base64"));
  };
  await command("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: "light" }] });
  await screenshot("inspection-desktop.png");
  await command("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: "dark" }] });
  await screenshot("inspection-dark.png");
  await command("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: "light" }, { name: "prefers-reduced-motion", value: "reduce" }] });
  await command("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await screenshot("inspection-mobile.png");
  if (await evaluate("document.documentElement.scrollWidth > innerWidth")) throw new Error("mobile feed overflows horizontally");
  await evaluate(`${revisionRoot}.querySelector('[data-zoom-preview]').click()`);
  await waitFor(`${revisionRoot}.querySelector('dialog.zoom')?.open`, "native zoom modal");
  const zoomContrast = await evaluate(`(() => {
    const style = getComputedStyle(${revisionRoot}.querySelector('dialog.zoom button'));
    const luminance = color => {
      const channels = color.match(/[0-9.]+/g).slice(0,3).map(Number).map(value => { const c=value/255; return c<=.04045 ? c/12.92 : ((c+.055)/1.055)**2.4; });
      return channels[0]*.2126 + channels[1]*.7152 + channels[2]*.0722;
    };
    const foreground=luminance(style.color), background=luminance(style.backgroundColor);
    return (Math.max(foreground,background)+.05)/(Math.min(foreground,background)+.05);
  })()`);
  if (zoomContrast < 4.5) throw new Error(`zoom button text contrast is only ${zoomContrast.toFixed(2)}:1`);
  for (let index = 0; index < 8; index += 1) {
    await command("Input.dispatchKeyEvent", { type: "keyDown", key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    await command("Input.dispatchKeyEvent", { type: "keyUp", key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    const backgroundFocused = await evaluate(`(() => { let active=document.activeElement; while(active?.shadowRoot?.activeElement) active=active.shadowRoot.activeElement; return active !== document.body && !active?.closest('dialog.zoom'); })()`);
    if (backgroundFocused) throw new Error("zoom keyboard focus escaped into background content");
  }
  await screenshot("inspection-zoom-mobile.png");
  await command("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await command("Input.dispatchKeyEvent", { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await waitFor(`!${revisionRoot}.querySelector('dialog.zoom')`, "zoom closes with Escape");
  if (!await evaluate(`${revisionRoot}.activeElement?.matches('[data-zoom-preview]')`)) throw new Error("zoom failed to restore focus");
  await evaluate(`${app}.querySelector('#post-${revision.post.id} [data-revision]').click()`);
  await waitFor(`${app}.activeElement?.id === 'post-${firstFigure.post.id}' && scrollY > 0`, "revision scroll and focus");

  const largeBytes = "x".repeat(16 * 1024 * 1024) + "FULL_DOCUMENT_END";
  const columns = Array.from({ length: 105 }, (_, index) => `column${index}`).join(",");
  const csv = `${columns}\n${Array.from({ length: 204 }, () => Array(105).fill("1").join(",")).join("\n")}\n\"unterminated`;
  const documents = await publishInspection("Large-file and CSV inspection", [{ name: "large.txt", bytes: largeBytes }, { name: "wide.csv", bytes: csv }]);
  await command("Emulation.setDeviceMetricsOverride", { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
  await command("Page.navigate", { url: `${daemonOrigin}/sessions/${inspection.session.public_id}` });
  const largeArtifact = `${app}?.querySelector('#post-${documents.post.id} glim-artifact')?.shadowRoot`;
  const csvArtifact = `${app}?.querySelector('#post-${documents.post.id} li:nth-child(2) glim-artifact')?.shadowRoot`;
  await waitFor(`${largeArtifact}?.querySelector('[data-load-full]')`, "large document opt-in");
  const largePath = `/api/v1/posts/${documents.post.id}/files/0/content`;
  if (await evaluate(`performance.getEntriesByType('resource').some(entry=>new URL(entry.name).pathname==='${largePath}')`)) throw new Error("large document downloaded without consent");
  await evaluate(`${app}.querySelector('#post-${documents.post.id} li:nth-child(2)').scrollIntoView()`);
  await waitFor(`${csvArtifact}?.querySelector('table')`, "CSV inspection");
  const csvState = await evaluate(`({text:${csvArtifact}.textContent,rows:${csvArtifact}.querySelectorAll('tr').length,columns:${csvArtifact}.querySelector('tr').children.length,fullscreen:!!${csvArtifact}.querySelector('[data-fullscreen]')})`);
  if (!csvState.text.includes("first 200 rows") || !csvState.text.includes("first 100 of 105 columns") || !csvState.text.includes("CSV parse issues") || csvState.rows !== 200 || csvState.columns !== 100 || !csvState.fullscreen) throw new Error(`CSV disclosure failed: ${JSON.stringify(csvState)}`);
  await evaluate(`${largeArtifact}.querySelector('[data-load-full]').click(); ${app}.querySelector('#post-${documents.post.id}').scrollIntoView()`);
  await waitFor(`${largeArtifact}?.querySelector('pre')?.textContent.endsWith('FULL_DOCUMENT_END')`, "complete opted-in document", 300);
  const purgeInspection = await fetch(`${daemonOrigin}/api/v1/sessions/${inspection.session.public_id}`, { method: "DELETE", headers: authenticatedHeaders() });
  if (!purgeInspection.ok) throw new Error("inspection session purge failed");
  await waitFor(`${app}?.querySelector('.state')?.textContent === 'Session closed'`, "inspection purge releases renderers");
  console.log(`Chromium inspection: ${JSON.stringify(loading)}; 16 MiB opt-in, CSV disclosure, native modal, revision navigation, responsive screenshots passed`);

  if (runtimeExceptions.length > 0) throw new Error(`browser runtime exceptions: ${JSON.stringify(runtimeExceptions)}`);
  console.log(`Chromium live feed: ${pdfBytes.length}-byte, 200-page native PDF plus capability isolation, insertion, queueing, closure, heartbeat, and confirmed close passed`);
} catch (error) {
  throw new Error(`${error.message}\nDaemon stderr:\n${daemonErrors}\nChromium stderr:\n${browserErrors}`);
} finally {
  socket?.close();
  browser?.kill("SIGTERM");
  daemon.kill("SIGTERM");
  await rm(profile, { recursive: true, force: true });
  await rm(storeRoot, { recursive: true, force: true });
}
