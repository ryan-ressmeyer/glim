import { spawn } from "node:child_process";
import { access, chmod, mkdtemp, open, readFile, rm, writeFile } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";

import { resolveAcceptanceConfiguration } from "./resource-acceptance-config.mjs";

if (process.platform !== "linux") throw new Error("resource acceptance requires Linux /proc");

const {
  fileBytes,
  feedPosts,
  timeoutMs,
  maxHwmBytes,
  maxHwmGrowthBytes,
  maxPageBytes,
} = resolveAcceptanceConfiguration(process.env);
const mib = 1024 * 1024;

const root = await mkdtemp(path.join(os.tmpdir(), "glim-resource-acceptance-"));
const daemonBinary = new URL("../target/debug/glim", import.meta.url).pathname;
await access(daemonBinary);
let daemon;
let curl;
let daemonStderr = "";

const reservePort = async () => {
  const server = net.createServer();
  await new Promise((resolve, reject) => server.listen(0, "127.0.0.1", resolve).once("error", reject));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("could not reserve a daemon port");
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  return address.port;
};

const hwmBytes = async (pid) => {
  const status = await readFile(`/proc/${pid}/status`, "utf8");
  const match = /^VmHWM:\s+(\d+)\s+kB$/m.exec(status);
  if (!match) throw new Error("daemon /proc status lacks VmHWM");
  return Number(match[1]) * 1024;
};

const stopProcess = async (child) => {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill("SIGTERM");
  for (let attempt = 0; attempt < 40; attempt += 1) {
    if (child.exitCode !== null || child.signalCode !== null) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  child.kill("SIGKILL");
  for (let attempt = 0; attempt < 40; attempt += 1) {
    if (child.exitCode !== null || child.signalCode !== null) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`process ${child.pid} did not exit after SIGKILL`);
};

const run = async () => {
  const port = await reservePort();
  const origin = `http://127.0.0.1:${port}`;
  const token = "c".repeat(64);
  const tokenPath = path.join(root, "token");
  const configPath = path.join(root, "config.json");
  const storePath = path.join(root, "store");
  await writeFile(tokenPath, token);
  await chmod(tokenPath, 0o600);
  await writeFile(configPath, JSON.stringify({
    schema_version: 1,
    store_root: storePath,
    bind: `127.0.0.1:${port}`,
    access: { mode: "token", token_file: tokenPath, public_origin: origin },
    limits: { max_upload_bytes: fileBytes + mib, max_finalized_blob_bytes: fileBytes + 8 * mib },
  }));
  const environment = { ...process.env, GLIM_CONFIG: configPath };
  for (const name of ["GLIM_STORE_ROOT", "GLIM_BIND", "GLIM_ACCESS_MODE", "GLIM_TOKEN_FILE", "GLIM_PUBLIC_ORIGIN", "GLIM_TLS_CERTIFICATE", "GLIM_TLS_PRIVATE_KEY", "GLIM_TRUSTED_PROXY_IPS", "GLIM_MAX_UPLOAD_BYTES", "GLIM_MAX_FINALIZED_BLOB_BYTES"]) delete environment[name];
  daemon = spawn(daemonBinary, ["daemon"], { env: environment, stdio: ["ignore", "ignore", "pipe"] });
  daemon.stderr.on("data", (chunk) => { daemonStderr += chunk; });
  for (let attempt = 0; attempt < 200; attempt += 1) {
    try { if ((await fetch(`${origin}/api/v1/health`)).ok) break; } catch {}
    if (attempt === 199) throw new Error("daemon did not become healthy");
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  const baselineHwm = await hwmBytes(daemon.pid);

  const largePath = path.join(root, "large.bin");
  const file = await open(largePath, "w");
  await file.truncate(fileBytes);
  await file.close();
  const manifestPath = path.join(root, "large-manifest.json");
  await writeFile(manifestPath, JSON.stringify({
    integration_namespace: "acceptance", external_key: "resource", project_label: "Acceptance",
    working_directory: "/tmp/glim-resource-acceptance", title: "Large stream", commentary: "Resident-memory acceptance",
    files: [{ part: "file", filename: "large.bin", support_assets: [] }],
  }));
  const curlOutput = [];
  curl = spawn("curl", ["--silent", "--show-error", "--fail-with-body", "-H", `Authorization: Bearer ${token}`,
    "-F", `manifest=<${manifestPath};type=application/json`, "-F", `file=@${largePath};filename=large.bin`, `${origin}/api/v1/posts`],
  { stdio: ["ignore", "pipe", "pipe"] });
  curl.stdout.on("data", (chunk) => curlOutput.push(chunk));
  let curlError = "";
  curl.stderr.on("data", (chunk) => { curlError += chunk; });
  const curlCode = await new Promise((resolve) => curl.once("close", resolve));
  curl = undefined;
  if (curlCode !== 0) throw new Error(`large streamed publication failed: ${curlError}`);
  const largePublication = JSON.parse(Buffer.concat(curlOutput).toString("utf8"));

  const auth = { authorization: `Bearer ${token}` };
  for (let index = 0; index < feedPosts; index += 1) {
    const boundary = `accept-${index}`;
    const manifest = JSON.stringify({
      integration_namespace: "acceptance", external_key: "resource", project_label: "Acceptance",
      working_directory: "/tmp/glim-resource-acceptance", title: `Feed ${index}`, commentary: "Bounded long feed",
      files: [{ part: "file", filename: `${index}.txt`, support_assets: [] }],
    });
    const body = `--${boundary}\r\nContent-Disposition: form-data; name="manifest"\r\n\r\n${manifest}\r\n--${boundary}\r\nContent-Disposition: form-data; name="file"\r\n\r\n${index}\r\n--${boundary}--\r\n`;
    const response = await fetch(`${origin}/api/v1/posts`, { method: "POST", headers: { ...auth, "content-type": `multipart/form-data; boundary=${boundary}` }, body });
    if (response.status !== 201) throw new Error(`feed publication ${index} failed: ${response.status} ${await response.text()}`);
  }

  const pageResponse = await fetch(`${origin}/api/v1/posts?limit=100`, { headers: auth });
  const pageBytes = Buffer.from(await pageResponse.arrayBuffer());
  if (!pageResponse.ok || pageBytes.length > maxPageBytes) throw new Error(`bounded page failed: status=${pageResponse.status} bytes=${pageBytes.length}`);
  const page = JSON.parse(pageBytes.toString("utf8"));
  const totalPosts = feedPosts + 1;
  const expectedPagePosts = Math.min(totalPosts, 100);
  const cursorIsBounded = totalPosts > 100 ? typeof page.next_cursor === "string" : page.next_cursor === null;
  if (page.posts.length !== expectedPagePosts || !cursorIsBounded) throw new Error(`long feed was not bounded: ${page.posts.length}`);

  const peakHwm = await hwmBytes(daemon.pid);
  const growth = peakHwm - baselineHwm;
  if (peakHwm > maxHwmBytes || growth > maxHwmGrowthBytes) {
    throw new Error(`resident-memory threshold exceeded: peak=${peakHwm} growth=${growth}`);
  }

  const close = await fetch(`${origin}/api/v1/sessions/${largePublication.session.public_id}`, { method: "DELETE", headers: auth });
  if (!close.ok) throw new Error(`session purge failed: ${close.status} ${await close.text()}`);
  const status = await (await fetch(`${origin}/api/v1/status`, { headers: auth })).json();
  if (status.active_sessions !== 0 || status.finalized_unique_blob_bytes !== 0 || status.queued_blob_deletions !== 0) {
    throw new Error(`purge left data: ${JSON.stringify(status)}`);
  }
  console.log(JSON.stringify({ file_bytes: fileBytes, feed_posts: feedPosts, page_posts: page.posts.length, page_bytes: pageBytes.length, baseline_hwm_bytes: baselineHwm, peak_hwm_bytes: peakHwm, hwm_growth_bytes: growth, max_hwm_bytes: maxHwmBytes, max_hwm_growth_bytes: maxHwmGrowthBytes }));
};

let timeout;
try {
  await Promise.race([run(), new Promise((_, reject) => {
    timeout = setTimeout(() => reject(new Error(`resource acceptance timed out after ${timeoutMs} ms`)), timeoutMs);
  })]);
} catch (error) {
  throw new Error(`${error.message}\nDaemon stderr:\n${daemonStderr}`);
} finally {
  clearTimeout(timeout);
  await stopProcess(curl);
  await stopProcess(daemon);
  await rm(root, { recursive: true, force: true });
}
