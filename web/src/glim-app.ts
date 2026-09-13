import { marked } from "marked";
import Papa from "papaparse";
import sanitizeHtml from "sanitize-html";

const API = "/api/v1";
const CSV_MAX_ROWS = 200;
const CSV_MAX_CELLS_PER_ROW = 100;
const PROVENANCE_CONCURRENCY = 4;
const PROVENANCE_RETRY_LIMIT = 3;
const DOCUMENT_RENDER_LIMIT = 16 * 1024 * 1024;
const DOCUMENT_CONCURRENCY = 3;
const DIFF_WORK_LIMIT = 250_000;
const DIFF_ROW_LIMIT = 4_000;
const JSON_FORMAT_DEPTH_LIMIT = 64;
const JSON_FORMAT_OUTPUT_LIMIT = 16 * 1024 * 1024;
const VIEWPORT_RENDER_MARGIN = "800px 0px";
const MEDIA_RELEASE_MARGIN = "1000px 0px";
// Live delivery is lossy beyond these bounds; reconciliation replaces accumulation.
const LIVE_PENDING_LIMIT = 100;
const HEARTBEAT_INTERVAL_MS = 30_000;
const PUBLIC_ID_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const INITIAL_PUBLIC_ID_LENGTH = 6;
const MAX_DATE_SECONDS = 8_640_000_000_000;
// Page-route IDs must remain exactly representable by browser JavaScript.
const MAX_BROWSER_SAFE_ID = 9_007_199_254_740_991;

type Route =
  | { kind: "login" }
  | { kind: "global" }
  | { kind: "session"; publicId: string }
  | { kind: "project"; projectId: number }
  | { kind: "invalid" };

type Renderer = "image" | "svg" | "pdf" | "video" | "audio" | "markdown" | "text" | "json" | "csv" | "html" | "download";

interface SupportAsset {
  relative_path: string;
}

interface PostFile {
  position: number;
  filename: string;
  caption: string | null;
  media_type: string;
  renderer: Renderer;
  blob: { byte_size: number };
  support_assets: SupportAsset[];
}

interface GitProvenance {
  root: string;
  branch: string | null;
  commit: string | null;
}

interface Post {
  id: number;
  session_id: number;
  session_public_id: string;
  title: string;
  commentary: string;
  predecessor_post_id: number | null;
  published_at: number;
  git: GitProvenance | null;
  files: PostFile[];
}

interface Project {
  id: number;
  label: string;
  working_directory: string;
}

interface Session {
  id: number;
  public_id: string;
  integration_namespace: string;
  external_key: string;
  project: Project;
  created_at: number;
  last_activity_at: number;
}

interface Page {
  posts: Post[];
  next_cursor: string | null;
}

interface ArtifactData {
  postId: number;
  file: PostFile;
}

interface ImageComparisonData {
  older: ArtifactData;
  newer: ArtifactData;
}

interface TextComparisonData {
  older: ArtifactData;
  newer: ArtifactData;
}

type DiffLine = { kind: "equal" | "removed" | "added"; older: string; newer: string };
type DiffResult = { lines: DiffLine[]; fallback: null | "work" | "rows" };
type JsonFormatResult = { text: string; fallback: null | "depth" | "size" };

function isPublicId(value: unknown): value is string {
  return typeof value === "string"
    && value.length >= INITIAL_PUBLIC_ID_LENGTH
    && Array.from(value).every((character) => PUBLIC_ID_ALPHABET.includes(character));
}

function isPositiveSafeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) > 0 && (value as number) <= MAX_BROWSER_SAFE_ID;
}

function isDateSeconds(value: unknown): value is number {
  return Number.isSafeInteger(value) && Math.abs(value as number) <= MAX_DATE_SECONDS;
}

function routeFromLocation(pathname: string): Route {
  if (pathname === "/login") return { kind: "login" };
  if (pathname === "/" || pathname === "/feed") return { kind: "global" };
  const sessionMatch = pathname.match(/^\/sessions\/([^/]+)$/);
  if (sessionMatch && isPublicId(sessionMatch[1])) return { kind: "session", publicId: sessionMatch[1] };
  const projectMatch = pathname.match(/^\/projects\/([1-9][0-9]*)$/);
  if (projectMatch) {
    const projectId = Number(projectMatch[1]);
    if (isPositiveSafeInteger(projectId)) return { kind: "project", projectId };
  }
  return { kind: "invalid" };
}

function comparisonPostId(hash: string): number | null {
  const match = hash.match(/^#compare-([1-9][0-9]*)$/);
  if (!match) return null;
  const postId = Number(match[1]);
  return isPositiveSafeInteger(postId) ? postId : null;
}

function pageEndpoint(route: Route): string | null {
  if (route.kind === "global") return `${API}/posts`;
  if (route.kind === "session") return `${API}/sessions/${route.publicId}/posts`;
  if (route.kind === "project") return `${API}/projects/${route.projectId}/posts`;
  return null;
}

function eventEndpoint(route: Route): string | null {
  const endpoint = pageEndpoint(route);
  return endpoint ? `${endpoint}/events` : null;
}

function element<K extends keyof HTMLElementTagNameMap>(name: K, text?: string): HTMLElementTagNameMap[K] {
  const value = document.createElement(name);
  if (text !== undefined) value.textContent = text;
  return value;
}

const SANITIZER_OPTIONS: sanitizeHtml.IOptions = {
  allowedTags: [
    "p", "br", "strong", "em", "del", "blockquote", "ul", "ol", "li",
    "h1", "h2", "h3", "h4", "h5", "h6", "pre", "code", "a", "img",
    "table", "thead", "tbody", "tr", "th", "td", "hr",
  ],
  allowedAttributes: {
    a: ["href", "title"],
    img: ["src", "alt", "title"],
    th: ["scope"],
  },
  allowedSchemes: ["http", "https", "mailto"],
  allowProtocolRelative: false,
  disallowedTagsMode: "discard",
};

function safeMarkdown(markdown: string, supportResolver?: (value: string) => string | null): string {
  const rendered = marked.parse(markdown, { async: false }) as string;
  const template = document.createElement("template");
  template.innerHTML = sanitizeHtml(rendered, SANITIZER_OPTIONS);
  for (const value of Array.from(template.content.querySelectorAll<HTMLElement>("a[href], img[src]"))) {
    const attribute = value.tagName === "A" ? "href" : "src";
    const raw = value.getAttribute(attribute) ?? "";
    const replacement = supportResolver?.(raw);
    if (replacement) {
      value.setAttribute(attribute, replacement);
      continue;
    }
    if (value.tagName === "A" && !supportResolver && (/^https?:\/\//i.test(raw) || /^mailto:/i.test(raw) || raw.startsWith("#"))) {
      continue;
    }
    value.removeAttribute(attribute);
  }
  return sanitizeHtml(template.innerHTML, SANITIZER_OPTIONS);
}

function supportResolver(postId: number, file: PostFile): (value: string) => string | null {
  const stored = new Set(file.support_assets.map((asset) => asset.relative_path));
  return (raw) => {
    const firstSegment = raw.split("/", 1)[0];
    if (!raw || firstSegment.includes(":") || raw.startsWith("/") || raw.startsWith("\\") || raw.includes("\\") || raw.startsWith("#")) return null;
    let decoded: string;
    try {
      decoded = decodeURIComponent(raw.split(/[?#]/, 1)[0]);
    } catch {
      return null;
    }
    if (!decoded || decoded.split("/").some((part) => !part || part === "." || part === "..") || !stored.has(decoded)) return null;
    const encoded = decoded.split("/").map(encodeURIComponent).join("/");
    return `${API}/posts/${postId}/files/${file.position}/support/${encoded}`;
  };
}

function artifactUrl(postId: number, position: number): string {
  return `${API}/posts/${postId}/files/${position}/content`;
}

function supportScope(postId: number, file: PostFile): string {
  return `${API}/posts/${postId}/files/${file.position}/support/`;
}

function htmlResourceResolver(file: PostFile, scope: string): (value: string) => string | null {
  const stored = new Set(file.support_assets.map((asset) => asset.relative_path));
  return (raw) => {
    if (!raw || raw !== raw.trim() || raw.includes("?") || raw.includes("\\") || raw.startsWith("#")) return null;
    const hashIndex = raw.indexOf("#");
    const path = hashIndex === -1 ? raw : raw.slice(0, hashIndex);
    const fragment = hashIndex === -1 ? "" : raw.slice(hashIndex);
    const firstSegment = path.split("/", 1)[0];
    if (!path || firstSegment.includes(":") || path.startsWith("/")) return null;
    let decoded: string;
    try {
      decoded = decodeURIComponent(path);
    } catch {
      return null;
    }
    if (!decoded || decoded.split("/").some((part) => !part || part === "." || part === "..")
      || !stored.has(decoded) || Array.from(fragment).some((character) => character.charCodeAt(0) < 32)) return null;
    const encoded = decoded.split("/").map(encodeURIComponent).join("/");
    return `${scope}${encoded}${fragment}`;
  };
}

function htmlCsp(supportPrefix: string, scripts: boolean): string {
  const scope = new URL(supportPrefix, window.location.href).href;
  const scriptSource = scripts ? `'unsafe-inline' ${scope}` : "'none'";
  return [
    "default-src 'none'",
    "base-uri 'none'",
    "connect-src 'none'",
    "form-action 'none'",
    "frame-src 'none'",
    "object-src 'none'",
    `script-src ${scriptSource}`,
    `style-src 'unsafe-inline' data: ${scope}`,
    `img-src data: ${scope}`,
    `media-src data: ${scope}`,
    `font-src data: ${scope}`,
    "worker-src 'none'",
  ].join("; ");
}

function safeDataResource(raw: string, kind: "image" | "media" | "style"): string | null {
  const lower = raw.toLowerCase();
  if (kind === "image" && lower.startsWith("data:image/")) return raw;
  if (kind === "media" && (lower.startsWith("data:audio/") || lower.startsWith("data:video/"))) return raw;
  if (kind === "style" && lower.startsWith("data:text/css")) return raw;
  return null;
}

function rewriteHtmlResource(
  value: string,
  resolveSupport: (value: string) => string | null,
  dataKind?: "image" | "media" | "style",
): string | null {
  return resolveSupport(value) ?? (dataKind ? safeDataResource(value, dataKind) : null);
}

function sanitizeHtmlDocument(source: string, data: ArtifactData, scripts: boolean, supportPrefix: string): string {
  const documentValue = new DOMParser().parseFromString(source, "text/html");
  const resolveSupport = htmlResourceResolver(data.file, supportPrefix);

  documentValue.querySelectorAll("base, iframe, frame, object, embed").forEach((value) => value.remove());
  documentValue.querySelectorAll("meta").forEach((value) => {
    const directive = value.getAttribute("http-equiv")?.trim().toLowerCase();
    const name = value.getAttribute("name")?.trim().toLowerCase();
    if (directive === "refresh" || directive === "content-security-policy" || name === "referrer") value.remove();
  });
  documentValue.querySelectorAll<HTMLElement>("form").forEach((form) => {
    form.removeAttribute("action");
    form.removeAttribute("method");
    form.removeAttribute("target");
  });
  documentValue.querySelectorAll<HTMLElement>("input, button, select, textarea").forEach((control) => {
    control.setAttribute("disabled", "");
    control.removeAttribute("formaction");
    control.removeAttribute("formtarget");
  });
  documentValue.querySelectorAll<HTMLAnchorElement>("a[href], area[href]").forEach((anchor) => {
    const href = anchor.getAttribute("href") ?? "";
    if (!href.startsWith("#")) anchor.removeAttribute("href");
    anchor.removeAttribute("target");
    anchor.removeAttribute("ping");
  });

  documentValue.querySelectorAll<HTMLLinkElement>("link").forEach((link) => {
    const isStylesheet = link.relList.contains("stylesheet");
    const replacement = isStylesheet
      ? rewriteHtmlResource(link.getAttribute("href") ?? "", resolveSupport, "style")
      : null;
    if (!isStylesheet || !replacement) link.remove();
    else link.setAttribute("href", replacement);
  });

  const srcRules: Array<[string, "image" | "media" | undefined]> = [
    ["script[src]", undefined],
    ["img[src]", "image"],
    ["source[src]", "media"],
    ["video[src]", "media"],
    ["audio[src]", "media"],
    ["track[src]", "media"],
    ["input[src]", "image"],
  ];
  for (const [selector, dataKind] of srcRules) {
    documentValue.querySelectorAll<HTMLElement>(selector).forEach((value) => {
      const replacement = rewriteHtmlResource(value.getAttribute("src") ?? "", resolveSupport, dataKind);
      if (replacement) value.setAttribute("src", replacement);
      else value.removeAttribute("src");
    });
  }
  documentValue.querySelectorAll<HTMLElement>("video[poster]").forEach((video) => {
    const replacement = rewriteHtmlResource(video.getAttribute("poster") ?? "", resolveSupport, "image");
    if (replacement) video.setAttribute("poster", replacement);
    else video.removeAttribute("poster");
  });
  documentValue.querySelectorAll<HTMLElement>("img[srcset], source[srcset]").forEach((value) => {
    const rewritten = (value.getAttribute("srcset") ?? "").split(",").flatMap((candidate) => {
      const parts = candidate.trim().split(/\s+/);
      const replacement = parts[0] ? resolveSupport(parts[0]) : null;
      return replacement ? [`${replacement}${parts.length > 1 ? ` ${parts.slice(1).join(" ")}` : ""}`] : [];
    });
    if (rewritten.length > 0) value.setAttribute("srcset", rewritten.join(", "));
    else value.removeAttribute("srcset");
  });

  const csp = documentValue.createElement("meta");
  csp.setAttribute("http-equiv", "Content-Security-Policy");
  csp.setAttribute("content", htmlCsp(supportPrefix, scripts));
  documentValue.head.prepend(csp);
  return `<!doctype html>\n${documentValue.documentElement.outerHTML}`;
}

function downloadLink(data: ArtifactData, label = `Open or download ${data.file.filename}`): HTMLAnchorElement {
  const link = element("a", label);
  link.href = artifactUrl(data.postId, data.file.position);
  link.download = data.file.filename;
  link.className = "download";
  return link;
}

const RENDERERS = new Set<unknown>([
  "image", "svg", "pdf", "video", "audio", "markdown", "text", "json", "csv", "html", "download",
]);

function isPostFile(value: unknown): value is PostFile {
  if (!value || typeof value !== "object") return false;
  const file = value as Record<string, unknown>;
  const blob = file.blob as Record<string, unknown> | undefined;
  return Number.isSafeInteger(file.position)
    && (file.position as number) >= 0
    && typeof file.filename === "string"
    && (file.caption === null || typeof file.caption === "string")
    && typeof file.media_type === "string"
    && RENDERERS.has(file.renderer)
    && !!blob
    && Number.isSafeInteger(blob.byte_size)
    && (blob.byte_size as number) >= 0
    && Array.isArray(file.support_assets)
    && file.support_assets.every((asset) => !!asset && typeof asset === "object" && typeof (asset as Record<string, unknown>).relative_path === "string");
}

function hasControlCharacter(value: string): boolean {
  return Array.from(value).some((character) => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127);
}

function isGitProvenance(value: unknown): value is GitProvenance {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const git = value as Record<string, unknown>;
  const rootValid = typeof git.root === "string"
    && git.root.startsWith("/")
    && git.root.length <= 4096
    && !hasControlCharacter(git.root);
  const branchValid = git.branch === null || (typeof git.branch === "string"
    && git.branch.trim().length > 0
    && git.branch.length <= 1024
    && !hasControlCharacter(git.branch));
  const commitValid = git.commit === null || (typeof git.commit === "string"
    && [40, 64].includes(git.commit.length)
    && Array.from(git.commit).every((character) => "0123456789abcdefABCDEF".includes(character)));
  return Object.keys(git).every((key) => ["root", "branch", "commit"].includes(key))
    && rootValid && branchValid && commitValid;
}

function isPost(value: unknown): value is Post {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const postValue = value as Record<string, unknown>;
  return Object.keys(postValue).every((key) => [
    "id", "session_id", "session_public_id", "title", "commentary", "predecessor_post_id",
    "published_at", "git", "files",
  ].includes(key))
    && isPositiveSafeInteger(postValue.id)
    && isPositiveSafeInteger(postValue.session_id)
    && isPublicId(postValue.session_public_id)
    && typeof postValue.title === "string"
    && typeof postValue.commentary === "string"
    && isDateSeconds(postValue.published_at)
    && (postValue.predecessor_post_id === null || isPositiveSafeInteger(postValue.predecessor_post_id))
    && (postValue.git === null || isGitProvenance(postValue.git))
    && Array.isArray(postValue.files)
    && postValue.files.every((file, index) => isPostFile(file) && file.position === index);
}

function isPage(value: unknown): value is Page {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const candidate = value as Record<string, unknown>;
  if (!Object.keys(candidate).every((key) => ["posts", "next_cursor"].includes(key))
    || !Array.isArray(candidate.posts)
    || !(candidate.next_cursor === null || (typeof candidate.next_cursor === "string" && candidate.next_cursor.length > 0))) return false;
  const sessionIds = new Map<string, number>();
  return candidate.posts.every((raw) => {
    if (!isPost(raw)) return false;
    const previousSessionId = sessionIds.get(raw.session_public_id);
    if (previousSessionId !== undefined && previousSessionId !== raw.session_id) return false;
    sessionIds.set(raw.session_public_id, raw.session_id);
    return true;
  });
}

function comparePosts(left: Post, right: Post): number {
  return right.published_at - left.published_at || right.id - left.id;
}

function isSession(value: unknown): value is Session {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const candidate = value as Record<string, unknown>;
  const project = candidate.project as Record<string, unknown> | undefined;
  return isPositiveSafeInteger(candidate.id)
    && isPublicId(candidate.public_id)
    && typeof candidate.integration_namespace === "string"
    && typeof candidate.external_key === "string"
    && !!project
    && isPositiveSafeInteger(project.id)
    && typeof project.label === "string"
    && typeof project.working_directory === "string"
    && isDateSeconds(candidate.created_at)
    && isDateSeconds(candidate.last_activity_at);
}

const DEFERRED_RENDERERS = new Set<Renderer>(["markdown", "text", "json", "csv", "html"]);
let documentLoads = 0;
const documentWaiters: Array<() => void> = [];

async function acquireDocumentLoad(signal: AbortSignal): Promise<boolean> {
  if (signal.aborted) return false;
  if (documentLoads < DOCUMENT_CONCURRENCY) {
    documentLoads += 1;
    return true;
  }
  return new Promise<boolean>((resolve) => {
    const start = () => {
      signal.removeEventListener("abort", abort);
      if (signal.aborted) resolve(false);
      else { documentLoads += 1; resolve(true); }
    };
    const abort = () => {
      const index = documentWaiters.indexOf(start);
      if (index >= 0) documentWaiters.splice(index, 1);
      resolve(false);
    };
    signal.addEventListener("abort", abort, { once: true });
    documentWaiters.push(start);
  });
}

function releaseDocumentLoad() {
  documentLoads -= 1;
  documentWaiters.shift()?.();
}

async function fetchDocumentText(data: ArtifactData, signal: AbortSignal): Promise<string> {
  if (!await acquireDocumentLoad(signal)) throw new DOMException("aborted", "AbortError");
  try {
    const aborted = new Promise<never>((_resolve, reject) => {
      signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")), { once: true });
    });
    const response = await Promise.race([fetch(artifactUrl(data.postId, data.file.position), { signal }), aborted]);
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return await Promise.race([response.text(), aborted]);
  } finally {
    releaseDocumentLoad();
  }
}

function formatJsonLexically(source: string): JsonFormatResult {
  let depth = 0;
  let lineStart = false;
  let output = "";
  let outputBytes = 0;
  const append = (value: string): boolean => {
    const remaining = JSON_FORMAT_OUTPUT_LIMIT - outputBytes;
    if (value.length > remaining) return false;
    let bytes = 0;
    for (let index = 0; index < value.length && bytes <= remaining; index += 1) {
      const code = value.charCodeAt(index);
      if (code <= 0x7f) bytes += 1;
      else if (code <= 0x7ff) bytes += 2;
      else if (code >= 0xd800 && code <= 0xdbff
        && value.charCodeAt(index + 1) >= 0xdc00 && value.charCodeAt(index + 1) <= 0xdfff) {
        bytes += 4;
        index += 1;
      } else bytes += 3;
    }
    if (bytes > remaining) return false;
    output += value;
    outputBytes += bytes;
    return true;
  };
  const indent = () => append(`\n${"  ".repeat(depth)}`);

  for (let index = 0; index < source.length; index += 1) {
    const character = source[index];
    if (/\s/.test(character)) continue;
    if (character === "]" || character === "}") {
      depth -= 1;
      if (lineStart) lineStart = false;
      else if (!indent()) return { text: source, fallback: "size" };
      if (!append(character)) return { text: source, fallback: "size" };
      continue;
    }
    if (lineStart) {
      if (!indent()) return { text: source, fallback: "size" };
      lineStart = false;
    }
    if (character === '"') {
      let end = index + 1;
      let escaped = false;
      for (; end < source.length; end += 1) {
        if (!escaped && source[end] === '"') break;
        escaped = !escaped && source[end] === "\\";
        if (source[end] !== "\\") escaped = false;
      }
      if (!append(source.slice(index, end + 1))) return { text: source, fallback: "size" };
      index = end;
    } else if (character === "[" || character === "{") {
      if (!append(character)) return { text: source, fallback: "size" };
      depth += 1;
      if (depth > JSON_FORMAT_DEPTH_LIMIT) return { text: source, fallback: "depth" };
      lineStart = true;
    } else if (character === ",") {
      if (!append(character)) return { text: source, fallback: "size" };
      lineStart = true;
    } else if (character === ":") {
      if (!append(": ")) return { text: source, fallback: "size" };
    } else {
      let end = index + 1;
      while (end < source.length && !/[\s\[\]{},:"]/.test(source[end])) end += 1;
      if (!append(source.slice(index, end))) return { text: source, fallback: "size" };
      index = end - 1;
    }
  }
  return { text: output, fallback: null };
}

function lineDiff(olderText: string, newerText: string): DiffResult {
  const older = olderText.split(/\r?\n/, DIFF_ROW_LIMIT + 1);
  const newer = newerText.split(/\r?\n/, DIFF_ROW_LIMIT + 1);
  if (older.length > DIFF_ROW_LIMIT || newer.length > DIFF_ROW_LIMIT) return { lines: [], fallback: "rows" };
  let prefix = 0;
  while (prefix < older.length && prefix < newer.length && older[prefix] === newer[prefix]) prefix += 1;
  let suffix = 0;
  while (suffix < older.length - prefix && suffix < newer.length - prefix
    && older[older.length - suffix - 1] === newer[newer.length - suffix - 1]) suffix += 1;
  const oldMiddle = older.slice(prefix, older.length - suffix);
  const newMiddle = newer.slice(prefix, newer.length - suffix);
  if (prefix + suffix + oldMiddle.length + newMiddle.length > DIFF_ROW_LIMIT) return { lines: [], fallback: "rows" };
  if (oldMiddle.length * newMiddle.length > DIFF_WORK_LIMIT) return { lines: [], fallback: "work" };
  const width = newMiddle.length + 1;
  const lengths = new Uint32Array((oldMiddle.length + 1) * width);
  for (let oldIndex = oldMiddle.length - 1; oldIndex >= 0; oldIndex -= 1) {
    for (let newIndex = newMiddle.length - 1; newIndex >= 0; newIndex -= 1) {
      const index = oldIndex * width + newIndex;
      lengths[index] = oldMiddle[oldIndex] === newMiddle[newIndex]
        ? lengths[(oldIndex + 1) * width + newIndex + 1] + 1
        : Math.max(lengths[(oldIndex + 1) * width + newIndex], lengths[index + 1]);
    }
  }
  const lines: DiffLine[] = older.slice(0, prefix).map((line) => ({ kind: "equal", older: line, newer: line }));
  let oldIndex = 0;
  let newIndex = 0;
  while (oldIndex < oldMiddle.length || newIndex < newMiddle.length) {
    if (oldIndex < oldMiddle.length && newIndex < newMiddle.length && oldMiddle[oldIndex] === newMiddle[newIndex]) {
      lines.push({ kind: "equal", older: oldMiddle[oldIndex], newer: newMiddle[newIndex] });
      oldIndex += 1;
      newIndex += 1;
    } else if (newIndex >= newMiddle.length
      || (oldIndex < oldMiddle.length
        && lengths[(oldIndex + 1) * width + newIndex] >= lengths[oldIndex * width + newIndex + 1])) {
      lines.push({ kind: "removed", older: oldMiddle[oldIndex], newer: "" });
      oldIndex += 1;
    } else {
      lines.push({ kind: "added", older: "", newer: newMiddle[newIndex] });
      newIndex += 1;
    }
  }
  older.slice(older.length - suffix).forEach((line) => lines.push({ kind: "equal", older: line, newer: line }));
  return { lines, fallback: null };
}

function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
}

const artifactStyles = `
  :host { display: block; }
  * { box-sizing: border-box; }
  img.preview { cursor: zoom-in; display: block; height: auto; max-width: 100%; width: auto; }
  button, a { font: inherit; }
  button { background: var(--surface, #fff); border: 1px solid var(--border, #cbd5e1); border-radius: .4rem; color: inherit; cursor: pointer; padding: .38rem .65rem; }
  a { color: var(--link, #2456a6); text-underline-offset: .15em; }
  button:focus-visible, a:focus-visible, [tabindex]:focus-visible { outline: 3px solid var(--focus, #2563eb); outline-offset: 2px; }
  .preview-control { background: transparent; border: 0; cursor: zoom-in; max-width: 100%; padding: 0; }
  .pane { background: var(--surface, #fff); border: 1px solid var(--border, #cbd5e1); max-height: 75vh; min-height: 10rem; overflow: auto; resize: vertical; }
  .pane:fullscreen, .table-wrap:fullscreen { background: var(--surface, #fff); color: inherit; max-height: none; padding: 1rem; }
  pre { margin: 0; min-width: max-content; padding: 1rem; white-space: pre; }
  .table-wrap { border: 1px solid var(--border, #cbd5e1); max-height: 60vh; min-height: 10rem; overflow: auto; }
  table { border-collapse: collapse; }
  th, td { border: 1px solid var(--border, #cbd5e1); padding: .35rem .55rem; text-align: left; white-space: pre-wrap; }
  .toolbar { align-items: center; display: flex; flex-wrap: wrap; gap: .5rem; justify-content: flex-end; margin-bottom: .5rem; }
  .artifact-toolbar { color: var(--muted, #526077); font-size: .85rem; justify-content: flex-start; }
  .artifact-toolbar .spacer { flex: 1; }
  .artifact-toolbar a { margin: 0; }
  .pending, .error { background: var(--surface-subtle, #f8fafc); border: 1px dashed var(--border, #94a3b8); min-height: 6rem; padding: 1rem; }
  .render-placeholder { align-items: center; display: flex; }
  .zoom { background: #0f172a; border: 0; color: #f8fafc; height: 100%; margin: 0; max-height: none; max-width: none; overflow: auto; padding: 4.5rem 2rem 2rem; width: 100%; }
  .zoom::backdrop { background: rgb(15 23 42 / 92%); }
  .zoom img { display: block; height: auto; max-width: none; width: auto; }
  .zoom-controls { align-items: center; background: #0f172a; display: flex; flex-wrap: wrap; gap: .5rem; left: 1rem; max-width: calc(100% - 2rem); padding: .5rem; position: fixed; right: 1rem; top: .5rem; z-index: 1; }
  .zoom-controls button { background: #202c43; border-color: #64748b; color: #f8fafc; }
  .zoom-output { min-width: 4rem; text-align: center; }
  .markdown { overflow-wrap: anywhere; }
  .media { display: block; max-height: 75vh; max-width: 100%; width: auto; }
  audio.media { width: min(100%, 40rem); }
  .pdf-frame { border: 1px solid var(--border, #cbd5e1); display: block; height: 70vh; min-height: 18rem; width: 100%; }
  .html-frame { border: 1px solid var(--border, #cbd5e1); display: block; height: min(60vh, 42rem); max-height: 75vh; min-height: 18rem; width: 100%; }
  .html-frame:fullscreen { border: 0; height: 100vh; max-height: none; width: 100vw; }
  .script-warning { background: #fff7ed; border: 1px solid #fdba74; color: #431407; margin-bottom: .75rem; padding: .75rem; }
  .script-warning button { background: #fff7ed; color: #431407; display: block; margin-top: .5rem; }
  .download { display: inline-block; margin-top: .5rem; }
  @media (max-width: 30rem) { .zoom { padding-top: 8rem; } }
`;

class GlimArtifact extends HTMLElement {
  data?: ArtifactData;
  private controller?: AbortController;
  private closeZoom?: (restoreFocus?: boolean) => void;
  private mediaObserver?: IntersectionObserver;
  private viewportObserver?: IntersectionObserver;
  private renderGeneration = 0;
  private loadFullDocument = false;

  connectedCallback() {
    const generation = ++this.renderGeneration;
    this.controller?.abort();
    this.viewportObserver?.disconnect();
    this.viewportObserver = undefined;
    this.releaseRichResources();
    this.destroyHtmlContexts();
    this.removeImageSources();
    this.closeZoom?.(false);
    const root = this.root();
    root.replaceChildren(root.querySelector("style")!);
    const data = this.data;
    if (!data) return;
    root.append(this.renderArtifactToolbar(data));
    if (DEFERRED_RENDERERS.has(data.file.renderer) && data.file.blob.byte_size > DOCUMENT_RENDER_LIMIT && !this.loadFullDocument) {
      this.renderLargeDocument(root, data, generation);
      return;
    }
    const begin = (pending?: HTMLElement) => {
      this.viewportObserver?.disconnect();
      this.viewportObserver = undefined;
      if (pending) pending.textContent = `Loading ${data.file.filename}`;
      this.render(generation).then(() => pending?.remove()).catch((error: unknown) => {
        pending?.remove();
        if (this.isRenderActive(generation)
          && !(error instanceof DOMException && error.name === "AbortError")) this.renderFailure();
      });
    };
    if (DEFERRED_RENDERERS.has(data.file.renderer) && typeof IntersectionObserver !== "undefined") {
      const pending = element("div", `Waiting to render ${data.file.filename}`);
      pending.className = "pending render-placeholder";
      root.append(pending);
      this.viewportObserver = new IntersectionObserver((entries) => {
        if (!entries.some((entry) => entry.isIntersecting) || !this.isRenderActive(generation)) return;
        begin(pending);
      }, { rootMargin: VIEWPORT_RENDER_MARGIN });
      this.viewportObserver.observe(this);
    } else begin();
  }

  disconnectedCallback() {
    this.renderGeneration += 1;
    this.controller?.abort();
    this.viewportObserver?.disconnect();
    this.viewportObserver = undefined;
    this.releaseRichResources();
    this.destroyHtmlContexts();
    this.removeImageSources();
    this.closeZoom?.(false);
  }

  private root(): ShadowRoot {
    if (!this.shadowRoot) {
      const root = this.attachShadow({ mode: "open" });
      const style = element("style");
      style.textContent = artifactStyles;
      root.append(style);
    }
    return this.shadowRoot!;
  }

  private isRenderActive(generation: number, signal?: AbortSignal): boolean {
    return generation === this.renderGeneration && this.isConnected && !signal?.aborted;
  }

  private renderArtifactToolbar(data: ArtifactData): HTMLElement {
    const toolbar = element("div");
    toolbar.className = "toolbar artifact-toolbar";
    toolbar.dataset.artifactToolbar = "";
    toolbar.append(element("span", `${data.file.media_type || "Unknown type"} · ${formatBytes(data.file.blob.byte_size)}`));
    const spacer = element("span");
    spacer.className = "spacer";
    const open = element("a", "Open");
    open.href = artifactUrl(data.postId, data.file.position);
    open.target = "_blank";
    open.rel = "noopener";
    const download = downloadLink(data, "Download");
    const copy = element("button", "Copy link");
    copy.type = "button";
    copy.dataset.copyLink = "";
    if (!navigator.clipboard?.writeText) {
      copy.textContent = "Copy unavailable";
      copy.disabled = true;
    } else {
      copy.addEventListener("click", () => {
        const url = new URL(artifactUrl(data.postId, data.file.position), window.location.href).href;
        void navigator.clipboard.writeText(url).then(() => { copy.textContent = "Copied"; }, () => { copy.textContent = "Copy failed"; });
      });
    }
    toolbar.append(spacer, open, download, copy);
    return toolbar;
  }

  private async fetchHtmlSupportPrefix(data: ArtifactData, generation: number): Promise<string | null> {
    const controller = new AbortController();
    this.controller = controller;
    const endpoint = `${API}/posts/${data.postId}/files/${data.file.position}/html-capability`;
    const response = await fetch(endpoint, { method: "POST", signal: controller.signal });
    if (!this.isRenderActive(generation, controller.signal)) return null;
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const payload: unknown = await response.json();
    if (!payload || typeof payload !== "object" || Array.isArray(payload)) throw new Error("malformed HTML capability");
    const value = payload as Record<string, unknown>;
    if (!Object.keys(value).every((key) => ["path_prefix", "expires_in_seconds"].includes(key))
      || typeof value.path_prefix !== "string"
      || !isPositiveSafeInteger(value.expires_in_seconds)) throw new Error("malformed HTML capability");
    const ordinary = supportScope(data.postId, data.file);
    const suffix = `/api/v1/posts/${data.postId}/files/${data.file.position}/support/`;
    const capability = value.path_prefix.startsWith("/cap/") && value.path_prefix.endsWith(suffix);
    const parsed = new URL(value.path_prefix, window.location.href);
    if (!(value.path_prefix === ordinary || capability)
      || parsed.origin !== window.location.origin || parsed.search || parsed.hash) throw new Error("malformed HTML capability");
    return value.path_prefix;
  }

  private async fetchText(data: ArtifactData, generation: number): Promise<string | null> {
    const controller = new AbortController();
    this.controller = controller;
    const text = await fetchDocumentText(data, controller.signal);
    return this.isRenderActive(generation, controller.signal) ? text : null;
  }

  private renderLargeDocument(root: ShadowRoot, data: ArtifactData, generation: number) {
    const pending = element("div");
    pending.className = "pending large-document";
    pending.append(element("p", `${data.file.filename} is ${formatBytes(data.file.blob.byte_size)}, above the 16 MiB automatic rendering limit.`));
    const load = element("button", "Load full document");
    load.type = "button";
    load.dataset.loadFull = "";
    load.addEventListener("click", () => {
      if (!this.isRenderActive(generation)) return;
      this.loadFullDocument = true;
      this.connectedCallback();
    });
    pending.append(load, downloadLink(data, `Download ${data.file.filename}`));
    root.append(pending);
  }

  private async render(generation: number) {
    const data = this.data;
    if (!data || !this.isRenderActive(generation)) return;
    const root = this.root();
    switch (data.file.renderer) {
      case "image":
      case "svg":
        this.renderImage(root, data);
        return;
      case "video":
      case "audio":
        this.renderMedia(root, data, generation);
        return;
      case "pdf":
        this.renderPdf(root, data);
        return;
      case "markdown": {
        const text = await this.fetchText(data, generation);
        if (text === null || !this.isRenderActive(generation)) return;
        const container = element("div");
        container.className = "markdown pane";
        container.tabIndex = 0;
        container.innerHTML = safeMarkdown(text, supportResolver(data.postId, data.file));
        const toolbar = element("div");
        toolbar.className = "toolbar";
        const fullscreen = element("button", "Fullscreen");
        fullscreen.type = "button";
        fullscreen.dataset.fullscreen = "";
        fullscreen.addEventListener("click", () => {
          const request = container.requestFullscreen?.();
          request?.catch(() => undefined);
        });
        toolbar.append(fullscreen);
        root.append(toolbar, container);
        return;
      }
      case "text": {
        const text = await this.fetchText(data, generation);
        if (text !== null && this.isRenderActive(generation)) this.renderPane(root, text);
        return;
      }
      case "json": {
        const text = await this.fetchText(data, generation);
        if (text === null || !this.isRenderActive(generation)) return;
        try {
          this.renderPane(root, JSON.stringify(JSON.parse(text), null, 2));
        } catch {
          const error = element("div", "Persisted JSON is malformed");
          error.className = "error";
          error.append(downloadLink(data));
          root.append(error);
        }
        return;
      }
      case "csv": {
        const text = await this.fetchText(data, generation);
        if (text !== null && this.isRenderActive(generation)) this.renderCsv(root, text, data);
        return;
      }
      case "html": {
        const text = await this.fetchText(data, generation);
        if (text === null || !this.isRenderActive(generation)) return;
        const supportPrefix = await this.fetchHtmlSupportPrefix(data, generation);
        if (supportPrefix !== null && this.isRenderActive(generation)) {
          this.renderHtml(root, text, data, generation, supportPrefix);
        }
        return;
      }
      case "download":
        return;
      default: {
        const pending = element("div", `Renderer pending for ${data.file.filename}`);
        pending.className = "pending";
        pending.append(downloadLink(data));
        root.append(pending);
      }
    }
  }

  private renderMedia(root: ShadowRoot, data: ArtifactData, generation: number) {
    const media = element(data.file.renderer === "video" ? "video" : "audio");
    const url = artifactUrl(data.postId, data.file.position);
    media.className = "media";
    media.controls = true;
    media.autoplay = false;
    media.src = url;
    media.addEventListener("error", () => {
      if (this.isRenderActive(generation) && media.hasAttribute("src")) this.renderFailure();
    });
    root.append(media);
    if (typeof IntersectionObserver === "undefined") return;
    this.mediaObserver = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          if (!media.hasAttribute("src")) {
            media.src = url;
            media.load();
          }
        } else {
          media.pause();
          media.removeAttribute("src");
          media.load();
        }
      }
    }, { rootMargin: MEDIA_RELEASE_MARGIN });
    this.mediaObserver.observe(media);
  }

  private renderPdf(root: ShadowRoot, data: ArtifactData) {
    const url = artifactUrl(data.postId, data.file.position);
    const frame = element("iframe");
    frame.className = "pdf-frame";
    frame.src = url;
    frame.loading = "lazy";
    frame.title = `PDF: ${data.file.filename}`;

    root.append(frame);
  }

  private releaseRichResources() {
    this.mediaObserver?.disconnect();
    this.mediaObserver = undefined;
    this.shadowRoot?.querySelectorAll<HTMLMediaElement>("video, audio").forEach((media) => {
      media.pause();
      media.removeAttribute("src");
      media.load();
    });
  }

  private renderImage(root: ShadowRoot, data: ArtifactData) {
    const image = element("img");
    image.className = "preview";
    image.src = artifactUrl(data.postId, data.file.position);
    image.alt = data.file.caption ? `${data.file.caption} (${data.file.filename})` : data.file.filename;
    image.addEventListener("error", () => this.renderFailure(), { once: true });
    const preview = element("button");
    preview.type = "button";
    preview.className = "preview-control";
    preview.dataset.zoomPreview = "";
    preview.setAttribute("aria-label", `Open full-resolution view of ${image.alt}`);
    preview.addEventListener("click", () => this.openZoom(data, preview));
    preview.append(image);
    root.append(preview);
  }

  private openZoom(data: ArtifactData, trigger: HTMLElement) {
    this.closeZoom?.(false);
    const root = this.root();
    const dialog = element("dialog");
    dialog.className = "zoom";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", `Full-resolution view of ${data.file.filename}`);
    const image = element("img");
    image.src = artifactUrl(data.postId, data.file.position);
    image.alt = data.file.caption ?? data.file.filename;
    let scale = 1;
    const controls = element("div");
    controls.className = "zoom-controls";
    const close = element("button", "Close");
    const fit = element("button", "Fit to window");
    const actual = element("button", "100%");
    const zoomIn = element("button", "Zoom in");
    const zoomOut = element("button", "Zoom out");
    const output = element("output", "100%");
    output.className = "zoom-output";
    output.setAttribute("aria-live", "polite");
    const applyScale = () => {
      if (image.naturalWidth) image.style.width = `${Math.round(image.naturalWidth * scale)}px`;
      const percentage = scale * 100;
      output.value = `${percentage < 1 ? percentage.toFixed(1) : Math.round(percentage)}%`;
    };
    const fitImage = () => {
      if (!image.naturalWidth || !image.naturalHeight) return;
      const width = (dialog.clientWidth || window.innerWidth) - 64;
      const height = (dialog.clientHeight || window.innerHeight) - controls.getBoundingClientRect().height - 48;
      scale = Math.min(1, Math.max(1, width) / image.naturalWidth, Math.max(1, height) / image.naturalHeight);
      applyScale();
    };
    fit.addEventListener("click", fitImage);
    actual.addEventListener("click", () => { scale = 1; applyScale(); });
    zoomIn.addEventListener("click", () => { scale = Math.min(4, scale + 0.25); applyScale(); });
    zoomOut.addEventListener("click", () => { scale = Math.max(0.01, scale - 0.25); applyScale(); });
    image.addEventListener("load", fitImage, { once: true });
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") this.closeZoom?.();
      else if (event.key === "+" || event.key === "=") zoomIn.click();
      else if (event.key === "-") zoomOut.click();
      else if (event.key === "0") actual.click();
      else if (event.key.toLowerCase() === "f") fit.click();
    };
    this.closeZoom = (restoreFocus = true) => {
      window.removeEventListener("keydown", onKey);
      image.removeAttribute("src");
      if (dialog.open) dialog.close();
      dialog.remove();
      this.closeZoom = undefined;
      if (restoreFocus && trigger.isConnected) trigger.focus();
    };
    dialog.addEventListener("cancel", (event) => { event.preventDefault(); this.closeZoom?.(); });
    close.addEventListener("click", () => this.closeZoom?.());
    controls.append(close, fit, actual, zoomOut, zoomIn, output);
    dialog.append(controls, image);
    root.append(dialog);
    window.addEventListener("keydown", onKey);
    if (typeof dialog.showModal === "function") dialog.showModal();
    else dialog.setAttribute("open", "");
  }

  private renderPane(root: ShadowRoot, text: string) {
    const toolbar = element("div");
    toolbar.className = "toolbar";
    const fullscreen = element("button", "Fullscreen");
    fullscreen.type = "button";
    fullscreen.dataset.fullscreen = "";
    const pane = element("div");
    pane.className = "pane";
    pane.style.resize = "vertical";
    pane.tabIndex = 0;
    fullscreen.addEventListener("click", () => {
      const request = pane.requestFullscreen?.();
      request?.catch(() => undefined);
    });
    toolbar.append(fullscreen);
    const pre = element("pre");
    pre.textContent = text;
    pane.append(pre);
    root.append(toolbar, pane);
  }

  private renderCsv(root: ShadowRoot, text: string, data: ArtifactData) {
    const parsed = Papa.parse<string[]>(text, { skipEmptyLines: false });
    const rows = parsed.data.slice(0, CSV_MAX_ROWS);
    const toolbar = element("div");
    toolbar.className = "toolbar";
    const fullscreen = element("button", "Fullscreen");
    fullscreen.type = "button";
    fullscreen.dataset.fullscreen = "";
    const wrapper = element("div");
    wrapper.className = "table-wrap";
    wrapper.style.resize = "vertical";
    wrapper.tabIndex = 0;
    fullscreen.addEventListener("click", () => {
      const request = wrapper.requestFullscreen?.();
      request?.catch(() => undefined);
    });
    toolbar.append(fullscreen);
    const table = element("table");
    table.setAttribute("aria-label", data.file.filename);
    rows.forEach((row, rowIndex) => {
      const tr = element("tr");
      row.slice(0, CSV_MAX_CELLS_PER_ROW).forEach((cell) => {
        const entry = element(rowIndex === 0 ? "th" : "td", cell);
        if (rowIndex === 0) entry.setAttribute("scope", "col");
        tr.append(entry);
      });
      table.append(tr);
    });
    wrapper.append(table);
    root.append(toolbar, wrapper);
    if (parsed.data.length > CSV_MAX_ROWS) root.append(element("p", `Showing the first ${CSV_MAX_ROWS} rows of ${parsed.data.length}`));
    const widest = parsed.data.reduce((maximum, row) => Math.max(maximum, row.length), 0);
    if (widest > CSV_MAX_CELLS_PER_ROW) {
      root.append(element("p", `Showing the first ${CSV_MAX_CELLS_PER_ROW} of ${widest} columns; wider cells remain available in the download.`));
    }
    if (parsed.errors.length > 0) {
      const issues = element("section");
      issues.className = "csv-errors";
      issues.append(element("h3", `CSV parse issues (${parsed.errors.length})`));
      const list = element("ul");
      for (const error of parsed.errors.slice(0, 10)) {
        list.append(element("li", `${error.code}${error.row === undefined ? "" : ` at row ${error.row + 1}`}: ${error.message}`));
      }
      issues.append(list);
      if (parsed.errors.length > 10) issues.append(element("p", `Showing the first 10 of ${parsed.errors.length} parse issues.`));
      root.append(issues);
    }
  }

  private renderHtml(
    root: ShadowRoot,
    source: string,
    data: ArtifactData,
    generation: number,
    supportPrefix: string,
  ) {
    const warning = element("div");
    warning.className = "script-warning";
    warning.dataset.scriptWarning = "";
    const scriptStatus = element("strong", "Scripts disabled");
    scriptStatus.dataset.scriptStatus = "";
    warning.append(scriptStatus, document.createTextNode(
      ". Enabling scripts lets this document navigate its own frame and thereby make a network request.",
    ));
    const enable = element("button", "Enable sandboxed scripts");
    enable.type = "button";
    enable.dataset.enableScripts = "";
    warning.append(enable);

    const createFrame = (scripts: boolean, frameSupportPrefix: string) => {
      const frame = element("iframe");
      frame.className = "html-frame";
      frame.title = `Rendered HTML: ${data.file.filename}`;
      frame.referrerPolicy = "no-referrer";
      frame.setAttribute("sandbox", scripts ? "allow-scripts" : "");
      frame.srcdoc = sanitizeHtmlDocument(source, data, scripts, frameSupportPrefix);
      return frame;
    };
    let frame = createFrame(false, supportPrefix);
    enable.addEventListener("click", () => {
      if (!this.isRenderActive(generation) || enable.disabled) return;
      enable.disabled = true;
      void this.fetchHtmlSupportPrefix(data, generation).then((scriptSupportPrefix) => {
        if (scriptSupportPrefix === null || !this.isRenderActive(generation)) return;
        const replacement = createFrame(true, scriptSupportPrefix);
        frame.srcdoc = "";
        frame.removeAttribute("src");
        frame.replaceWith(replacement);
        frame = replacement;
        scriptStatus.textContent = "Sandboxed scripts enabled";
      }).catch(() => {
        if (this.isRenderActive(generation)) {
          enable.disabled = false;
          scriptStatus.textContent = "Scripts remain disabled because enabling them failed";
        }
      });
    });
    const toolbar = element("div");
    toolbar.className = "toolbar";
    const fullscreen = element("button", "Fullscreen document");
    fullscreen.type = "button";
    fullscreen.dataset.fullscreen = "";
    fullscreen.addEventListener("click", () => {
      const request = frame.requestFullscreen?.();
      request?.catch(() => undefined);
    });
    toolbar.append(fullscreen);
    root.append(warning, toolbar, frame);
  }

  private destroyHtmlContexts() {
    this.shadowRoot?.querySelectorAll<HTMLIFrameElement>("iframe").forEach((frame) => {
      frame.srcdoc = "";
      frame.removeAttribute("src");
      frame.remove();
    });
  }

  private removeImageSources() {
    this.shadowRoot?.querySelectorAll("img[src]").forEach((image) => image.removeAttribute("src"));
  }

  private renderFailure() {
    const data = this.data;
    if (!data || !this.isConnected) return;
    this.releaseRichResources();
    this.destroyHtmlContexts();
    const root = this.root();
    root.replaceChildren(root.querySelector("style")!);
    root.append(this.renderArtifactToolbar(data));
    const error = element("div", `Could not render ${data.file.filename}`);
    error.className = "error";
    const retry = element("button", "Retry rendering");
    retry.type = "button";
    retry.dataset.renderRetry = "";
    retry.addEventListener("click", () => this.connectedCallback());
    error.append(element("br"), retry, document.createTextNode(" "), downloadLink(data));
    root.append(error);
  }
}

class GlimTextComparison extends HTMLElement {
  data?: TextComparisonData;
  private controller?: AbortController;
  private observer?: IntersectionObserver;
  private generation = 0;
  private loadFull = false;

  connectedCallback() {
    const generation = ++this.generation;
    this.controller?.abort();
    this.observer?.disconnect();
    this.observer = undefined;
    this.replaceChildren();
    const data = this.data;
    if (!data) return;
    this.dataset.textComparison = "";
    const oversized = [data.older, data.newer].filter((artifact) => artifact.file.blob.byte_size > DOCUMENT_RENDER_LIMIT);
    if (oversized.length > 0 && !this.loadFull) {
      const pending = element("div");
      pending.className = "target-state";
      pending.append(element("p", `${oversized.map((artifact) => `${artifact.file.filename || "Unnamed artifact"} (${formatBytes(artifact.file.blob.byte_size)})`).join(" and ")} exceed the 16 MiB automatic rendering limit.`));
      const load = element("button", "Load full documents");
      load.type = "button";
      load.dataset.loadFullComparison = "";
      load.addEventListener("click", () => {
        if (generation !== this.generation || !this.isConnected) return;
        this.loadFull = true;
        this.connectedCallback();
      });
      pending.append(load, this.renderArtifactLinks(data));
      this.append(pending);
      return;
    }
    const begin = () => {
      this.observer?.disconnect();
      this.observer = undefined;
      this.replaceChildren(element("p", "Loading documents for comparison"), this.renderArtifactLinks(data));
      void this.load(generation, data);
    };
    if (typeof IntersectionObserver === "undefined") begin();
    else {
      const pending = element("p", "Waiting to render document comparison");
      this.append(pending, this.renderArtifactLinks(data));
      this.observer = new IntersectionObserver((entries) => {
        if (entries.some((entry) => entry.isIntersecting) && generation === this.generation && this.isConnected) begin();
      }, { rootMargin: VIEWPORT_RENDER_MARGIN });
      this.observer.observe(this);
    }
  }

  disconnectedCallback() {
    this.generation += 1;
    this.controller?.abort();
    this.observer?.disconnect();
    this.observer = undefined;
  }

  private async load(generation: number, data: TextComparisonData) {
    const controller = new AbortController();
    this.controller = controller;
    try {
      const [oldSource, newSource] = await Promise.all([
        fetchDocumentText(data.older, controller.signal),
        fetchDocumentText(data.newer, controller.signal),
      ]);
      if (generation !== this.generation || !this.isConnected || controller.signal.aborted) return;
      let olderText = oldSource;
      let newerText = newSource;
      const malformed: string[] = [];
      const formatFallbacks: Array<{ side: string; reason: "depth" | "size" }> = [];
      if (data.older.file.renderer === "json") {
        try {
          JSON.parse(oldSource);
          const formatted = formatJsonLexically(oldSource);
          olderText = formatted.text;
          if (formatted.fallback) formatFallbacks.push({ side: "previous", reason: formatted.fallback });
        } catch { malformed.push("previous"); }
      }
      if (data.newer.file.renderer === "json") {
        try {
          JSON.parse(newSource);
          const formatted = formatJsonLexically(newSource);
          newerText = formatted.text;
          if (formatted.fallback) formatFallbacks.push({ side: "current", reason: formatted.fallback });
        } catch { malformed.push("current"); }
      }
      const result = lineDiff(olderText, newerText);
      this.replaceChildren(this.renderArtifactLinks(data));
      if (malformed.length > 0) {
        const warning = element("p", `Malformed JSON in the ${malformed.join(" and ")} artifact; comparing disclosed raw text instead.`);
        warning.dataset.jsonFallback = "";
        this.append(warning);
      }
      for (const fallback of formatFallbacks) {
        const reason = fallback.reason === "depth"
          ? `${JSON_FORMAT_DEPTH_LIMIT}-level JSON formatting depth limit`
          : `${formatBytes(JSON_FORMAT_OUTPUT_LIMIT)} JSON formatting output limit`;
        const warning = element("p", `The ${fallback.side} artifact exceeds the ${reason}; comparing disclosed raw text instead.`);
        warning.dataset.jsonFormatFallback = fallback.reason;
        this.append(warning);
      }
      if (result.fallback) {
        const reason = result.fallback === "work"
          ? `${DIFF_WORK_LIMIT.toLocaleString()} comparison-cell work budget`
          : `${DIFF_ROW_LIMIT.toLocaleString()} rendered-row budget`;
        const notice = element("p", `Line diff exceeds the ${reason}; showing full plain text side by side.`);
        notice.dataset.diffFallback = result.fallback;
        this.append(notice, this.renderPlain(data, olderText, newerText));
      } else this.append(this.renderDiff(data, result.lines));
    } catch (error) {
      if (generation !== this.generation || !this.isConnected || controller.signal.aborted
        || (error instanceof DOMException && error.name === "AbortError")) return;
      const failure = element("p", "Could not load documents for comparison");
      failure.className = "target-state";
      this.replaceChildren(failure, this.renderArtifactLinks(data));
    }
  }

  private formatLabel(side: "Previous" | "Current", data: ArtifactData): string {
    return `${side} · ${data.file.renderer}`;
  }

  private renderArtifactLinks(data: TextComparisonData): HTMLElement {
    const navigation = element("nav");
    navigation.className = "comparison-document-links";
    navigation.setAttribute("aria-label", "Compared document links");
    for (const [label, artifact] of [["Previous", data.older], ["Current", data.newer]] as const) {
      const group = element("span", `${label}: `);
      const open = element("a", "Open");
      open.href = artifactUrl(artifact.postId, artifact.file.position);
      open.target = "_blank";
      open.rel = "noopener";
      group.append(open, document.createTextNode(" · "), downloadLink(artifact, "Download"));
      navigation.append(group);
    }
    return navigation;
  }

  private renderPlain(data: TextComparisonData, olderText: string, newerText: string): HTMLElement {
    const columns = element("div");
    columns.className = "comparison-columns";
    for (const [label, artifact, text] of [["Previous", data.older, olderText], ["Current", data.newer, newerText]] as const) {
      const side = element("section");
      side.append(element("h3", this.formatLabel(label, artifact)));
      const pre = element("pre", text);
      pre.className = "plain-comparison";
      side.append(pre);
      columns.append(side);
    }
    return columns;
  }

  private renderDiff(data: TextComparisonData, lines: DiffLine[]): HTMLElement {
    const wrapper = element("div");
    wrapper.className = "diff-wrap";
    const table = element("table");
    table.setAttribute("aria-label", `Line comparison of ${data.older.file.filename} and ${data.newer.file.filename}`);
    const head = element("thead");
    const heading = element("tr");
    heading.append(element("th", this.formatLabel("Previous", data.older)), element("th", this.formatLabel("Current", data.newer)));
    head.append(heading);
    const body = element("tbody");
    lines.forEach((line) => {
      const row = element("tr");
      row.dataset.diffRow = "";
      row.dataset.diffKind = line.kind;
      const previous = element("td", line.older);
      previous.dataset.diffSide = "Previous";
      const current = element("td", line.newer);
      current.dataset.diffSide = "Current";
      row.append(previous, current);
      body.append(row);
    });
    table.append(head, body);
    wrapper.append(table);
    return wrapper;
  }

}

class GlimImageComparison extends HTMLElement {
  data?: ImageComparisonData;

  connectedCallback() {
    this.replaceChildren();
    const data = this.data;
    if (!data) return;
    this.dataset.imageComparison = "";
    this.tabIndex = 0;
    this.setAttribute("aria-label", `Image comparison of ${data.older.file.filename} and ${data.newer.file.filename}`);
    let scale = 1;
    const controls = element("div");
    controls.className = "comparison-image-controls";
    const fit = element("button", "Fit both");
    fit.type = "button";
    fit.dataset.compareFit = "";
    const actual = element("button", "100%");
    actual.type = "button";
    actual.dataset.compareActual = "";
    const zoomOut = element("button", "Zoom out");
    zoomOut.type = "button";
    zoomOut.dataset.compareZoomOut = "";
    const zoomIn = element("button", "Zoom in");
    zoomIn.type = "button";
    zoomIn.dataset.compareZoomIn = "";
    const output = element("output", "100%");
    output.dataset.compareZoom = "";
    output.setAttribute("aria-live", "polite");
    const panes: HTMLElement[] = [];
    const images: HTMLImageElement[] = [];
    const applyScale = () => {
      images.forEach((image) => {
        if (image.naturalWidth) image.style.width = `${Math.round(image.naturalWidth * scale)}px`;
      });
      const percentage = scale * 100;
      output.value = `${percentage < 1 ? percentage.toFixed(1) : Math.round(percentage)}%`;
    };
    const fitBoth = () => {
      if (images.some((image) => !image.naturalWidth || !image.naturalHeight)) return;
      scale = Math.min(1, ...images.flatMap((image, index) => [
        Math.max(1, panes[index].clientWidth) / image.naturalWidth,
        Math.max(1, panes[index].clientHeight) / image.naturalHeight,
      ]));
      applyScale();
    };
    fit.addEventListener("click", fitBoth);
    actual.addEventListener("click", () => { scale = 1; applyScale(); });
    zoomOut.addEventListener("click", () => { scale = Math.max(0.01, scale - 0.25); applyScale(); });
    zoomIn.addEventListener("click", () => { scale = Math.min(4, scale + 0.25); applyScale(); });
    controls.append(fit, actual, zoomOut, zoomIn, output);
    const columns = element("div");
    columns.className = "comparison-columns";
    for (const [label, artifact] of [["Previous", data.older], ["Current", data.newer]] as const) {
      const side = element("section");
      side.append(element("h3", `${label}: ${artifact.file.filename || "Unnamed artifact"}`));
      const artifactToolbar = element("div");
      artifactToolbar.className = "comparison-artifact-toolbar";
      artifactToolbar.append(element("span", `${artifact.file.media_type || "Unknown type"} · ${formatBytes(artifact.file.blob.byte_size)}`));
      const open = element("a", "Open");
      open.href = artifactUrl(artifact.postId, artifact.file.position);
      open.target = "_blank";
      open.rel = "noopener";
      artifactToolbar.append(open, downloadLink(artifact, "Download"));
      side.append(artifactToolbar);
      const pane = element("div");
      pane.className = "comparison-image-pane";
      pane.dataset.imagePane = "";
      pane.tabIndex = 0;
      const image = element("img");
      image.dataset.compareImage = "";
      image.src = artifactUrl(artifact.postId, artifact.file.position);
      image.alt = artifact.file.caption ?? (artifact.file.filename || "Unnamed artifact");
      image.addEventListener("load", fitBoth);
      pane.append(image);
      side.append(pane);
      panes.push(pane);
      images.push(image);
      columns.append(side);
    }
    this.addEventListener("keydown", (event) => {
      if (event.key === "+" || event.key === "=") zoomIn.click();
      else if (event.key === "-") zoomOut.click();
      else if (event.key === "0") actual.click();
      else if (event.key.toLowerCase() === "f") fit.click();
    });
    this.append(controls, columns);
  }

  disconnectedCallback() {
    this.querySelectorAll("img[src]").forEach((image) => image.removeAttribute("src"));
  }
}

const appStyles = `
  :host {
    --background: #f6f7f9; --surface: #fff; --surface-subtle: #f1f3f6; --border: #d9dee7;
    --text: #172033; --muted: #5b6679; --link: #2456a6; --focus: #2563eb; --danger: #9f1239;
    background: var(--background); color: var(--text); color-scheme: light dark; display: block;
    font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; min-height: 100vh;
  }
  * { box-sizing: border-box; }
  main { margin: 0 auto; max-width: 76rem; padding: 1rem 1.5rem 3rem; }
  header { align-items: center; display: grid; gap: .5rem 1rem; grid-template-columns: auto 1fr auto; margin-bottom: 1.25rem; }
  h1 { font-size: 1.35rem; letter-spacing: -.02em; margin: 0; }
  h2 { font-size: clamp(1.25rem, 3vw, 1.65rem); line-height: 1.2; }
  nav { align-items: center; display: flex; flex-wrap: wrap; gap: .75rem; }
  a { color: var(--link); text-underline-offset: .15em; }
  button, summary { font: inherit; }
  button { background: var(--surface); border: 1px solid var(--border); border-radius: .4rem; color: var(--text); cursor: pointer; padding: .42rem .7rem; }
  button:hover { background: var(--surface-subtle); }
  button:disabled { cursor: default; opacity: .55; }
  a:focus-visible, button:focus-visible, summary:focus-visible, article:focus-visible { outline: 3px solid var(--focus); outline-offset: 3px; }
  article { background: var(--surface); border: 1px solid var(--border); border-radius: .65rem; margin-bottom: 1rem; padding: clamp(1rem, 3vw, 1.75rem); scroll-margin-top: 1rem; }
  article h2 { margin: 0 0 .35rem; }
  .commentary { line-height: 1.6; overflow-wrap: anywhere; }
  .meta, .provenance, .connection-status { color: var(--muted); font-size: .875rem; }
  .connection-status::before { content: "●"; font-size: .65em; margin-right: .4rem; }
  .provenance { border-top: 1px solid var(--border); margin-top: 1rem; padding-top: .7rem; }
  .provenance summary { cursor: pointer; width: max-content; }
  .provenance p { margin: .5rem 0 0; overflow-wrap: anywhere; white-space: pre-line; }
  .files { list-style: none; margin: 1.25rem 0 0; padding: 0; }
  .files > li { border-top: 1px solid var(--border); padding: 1rem 0; }
  .filename { font-weight: 650; overflow-wrap: anywhere; }
  .caption { margin: .35rem 0 .75rem; white-space: pre-wrap; }
  .state { background: var(--surface); border: 1px solid var(--border); border-radius: .65rem; padding: 2rem; text-align: center; }
  .live-notice { background: var(--surface); border: 1px solid var(--border); border-radius: .5rem; box-shadow: 0 .3rem 1.25rem rgb(15 23 42 / 15%); padding: .5rem; position: fixed; right: 1rem; top: .75rem; z-index: 2; }
  nav details { position: relative; }
  nav summary { cursor: pointer; }
  .action-menu { background: var(--surface); border: 1px solid var(--border); border-radius: .5rem; display: grid; gap: .4rem; padding: .5rem; position: absolute; right: 0; top: calc(100% + .4rem); width: max-content; z-index: 3; }
  .danger { color: var(--danger); }
  .target-state { background: var(--surface-subtle); border: 1px solid var(--border); padding: .75rem; }
  [data-comparison] { background: var(--surface); border: 1px solid var(--border); border-radius: .65rem; padding: clamp(1rem, 3vw, 1.75rem); }
  .comparison-selectors { display: flex; flex-wrap: wrap; gap: .75rem; margin: 1rem 0; }
  .comparison-selectors label { display: grid; gap: .25rem; }
  .comparison-columns { display: grid; gap: 1rem; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); margin-top: 1rem; }
  .comparison-columns > section { min-width: 0; }
  .comparison-image-controls, .comparison-artifact-toolbar, .comparison-document-links { align-items: center; display: flex; flex-wrap: wrap; gap: .5rem; margin-top: 1rem; }
  .comparison-document-links { justify-content: space-between; }
  .comparison-artifact-toolbar { color: var(--muted); font-size: .85rem; }
  .comparison-artifact-toolbar span { margin-right: auto; }
  .diff-wrap, .plain-comparison { border: 1px solid var(--border); max-height: 70vh; overflow: auto; }
  .diff-wrap table { border-collapse: collapse; width: 100%; }
  .diff-wrap th, .diff-wrap td { border: 1px solid var(--border); padding: .25rem .45rem; text-align: left; vertical-align: top; white-space: pre-wrap; width: 50%; }
  .diff-wrap tr[data-diff-kind="removed"] td:first-child, .diff-wrap tr[data-diff-kind="added"] td:last-child { background: var(--surface-subtle); }
  .plain-comparison { margin: 0; min-height: 10rem; padding: .75rem; resize: vertical; white-space: pre; }
  .comparison-image-pane { border: 1px solid var(--border); height: min(65vh, 42rem); overflow: auto; }
  .comparison-image-pane img { display: block; height: auto; max-width: none; width: auto; }
  @media (prefers-color-scheme: dark) {
    :host { --background: #101318; --surface: #171b22; --surface-subtle: #202631; --border: #343c49; --text: #edf1f7; --muted: #aab4c3; --link: #91b9ff; --focus: #70a5ff; --danger: #fda4af; }
  }
  @media (max-width: 42rem) {
    main { padding: .75rem; }
    header { grid-template-columns: 1fr auto; }
    nav { grid-column: 1 / -1; }
    article, [data-comparison] { border-radius: 0; margin-inline: -.75rem; }
    .comparison-columns { grid-template-columns: minmax(0, 1fr); }
    .diff-wrap thead { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); }
    .diff-wrap tr, .diff-wrap td { display: block; width: auto; }
    .diff-wrap td::before { content: attr(data-diff-side) ": "; color: var(--muted); font-size: .8rem; }
  }
  @media (prefers-reduced-motion: reduce) { * { scroll-behavior: auto !important; } }
`;

class GlimApp extends HTMLElement {
  private route: Route = { kind: "invalid" };
  private posts: Post[] = [];
  private postIds = new Set<number>();
  private nextCursor: string | null = null;
  private sessions = new Map<string, Session>();
  private provenanceUnavailable = new Set<string>();
  private provenanceAttempts = new Map<string, number>();
  private provenanceInFlight = new Map<string, Promise<Session | null>>();
  private provenanceActive = 0;
  private provenanceWaiters: Array<() => void> = [];
  private controller?: AbortController;
  private main?: HTMLElement;
  private navigation?: HTMLElement;
  private connectionStatus?: HTMLElement;
  private connectionGeneration = 0;
  private events?: EventSource;
  private pendingPosts = new Map<number, Post>();
  private heartbeatTimer?: number;
  private heartbeatController?: AbortController;
  private heartbeatInFlight = false;
  private streamOpen = false;
  private reconciliationVersion = 0;
  private reconciliationController?: AbortController;
  private reconciling = false;
  private needsLiveReconciliation = false;
  private closed = false;
  private authenticationExpired = false;
  private targetRequestVersion = 0;
  private targetController?: AbortController;
  private targetHash?: string;
  private focusedHash: string | null = null;
  private readonly visibilityHandler = () => this.syncHeartbeat();
  private readonly hashHandler = () => {
    const requestedHash = window.location.hash;
    if (this.targetController && !this.targetController.signal.aborted && this.targetHash === requestedHash) return;
    this.focusedHash = null;
    this.targetRequestVersion += 1;
    this.targetController?.abort();
    this.targetController = undefined;
    if (comparisonPostId(window.location.hash) === null) {
      this.main?.querySelector("[data-comparison]")?.remove();
      if (this.posts.length > 0) this.renderFeed();
    }
    void this.resolveLocationPost(this.connectionGeneration);
  };

  constructor() {
    super();
    const root = this.attachShadow({ mode: "open" });
    const style = element("style");
    style.textContent = appStyles;
    root.append(style);
  }

  connectedCallback() {
    const generation = ++this.connectionGeneration;
    this.controller?.abort();
    this.targetRequestVersion += 1;
    this.targetController?.abort();
    this.targetController = undefined;
    this.focusedHash = null;
    this.route = routeFromLocation(window.location.pathname);
    this.renderShell();
    if (this.route.kind === "invalid") {
      this.showState("Page not found");
      return;
    }
    if (this.route.kind === "login") {
      this.renderLogin(generation);
      return;
    }
    this.closed = false;
    this.authenticationExpired = false;
    window.addEventListener("hashchange", this.hashHandler);
    this.startLive(generation);
    this.load(false, generation).catch(() => undefined);
  }

  disconnectedCallback() {
    this.connectionGeneration += 1;
    this.controller?.abort();
    this.targetRequestVersion += 1;
    this.targetController?.abort();
    this.targetController = undefined;
    window.removeEventListener("hashchange", this.hashHandler);
    this.stopLive();
  }

  private isAppActive(generation: number, signal?: AbortSignal): boolean {
    return generation === this.connectionGeneration && this.isConnected && !signal?.aborted;
  }

  private startLive(generation: number) {
    const endpoint = eventEndpoint(this.route);
    if (!endpoint || typeof EventSource === "undefined") {
      this.setConnectionStatus("Updates unavailable");
      return;
    }
    this.setConnectionStatus("Connecting");
    this.events?.close();
    const source = new EventSource(endpoint);
    this.events = source;
    source.onopen = () => {
      if (!this.isAppActive(generation) || this.events !== source) return;
      this.streamOpen = true;
      this.setConnectionStatus("Live");
      this.syncHeartbeat();
    };
    source.onerror = () => {
      if (!this.isAppActive(generation) || this.events !== source) return;
      this.streamOpen = false;
      this.setConnectionStatus("Reconnecting");
      this.stopHeartbeat();
      this.reconcileLatest(generation);
    };
    source.addEventListener("post", ((event: MessageEvent<string>) => {
      if (!this.isAppActive(generation) || this.events !== source || this.closed) return;
      let value: unknown;
      try { value = JSON.parse(event.data); } catch { this.reconcileLatest(generation); return; }
      if (!isPost(value) || (event.lastEventId !== "" && event.lastEventId !== String(value.id))) {
        this.reconcileLatest(generation);
        return;
      }
      this.receivePost(value, generation);
    }) as EventListener);
    source.addEventListener("reset", (() => {
      if (this.isAppActive(generation) && this.events === source) this.reconcileLatest(generation);
    }) as EventListener);
    source.addEventListener("session-closed", ((event: MessageEvent<string>) => {
      if (!this.isAppActive(generation) || this.events !== source) return;
      let value: unknown;
      try { value = JSON.parse(event.data); } catch { this.reconcileLatest(generation); return; }
      if (!value || typeof value !== "object") { this.reconcileLatest(generation); return; }
      const closure = value as Record<string, unknown>;
      if (!isPositiveSafeInteger(closure.project_id) || !isPublicId(closure.session_public_id)) {
        this.reconcileLatest(generation);
        return;
      }
      if (this.route.kind === "session" && closure.session_public_id === this.route.publicId) {
        this.showClosed();
        return;
      }
      this.posts = this.posts.filter((post) => post.session_public_id !== closure.session_public_id);
      this.postIds = new Set(this.posts.map((post) => post.id));
      for (const [id, post] of this.pendingPosts) {
        if (post.session_public_id === closure.session_public_id) this.pendingPosts.delete(id);
      }
      this.renderFeed();
      this.reconcileLatest(generation);
    }) as EventListener);
    document.addEventListener("visibilitychange", this.visibilityHandler);
  }

  private stopLive() {
    this.events?.close();
    this.events = undefined;
    this.streamOpen = false;
    document.removeEventListener("visibilitychange", this.visibilityHandler);
    this.stopHeartbeat();
    this.reconciliationVersion += 1;
    this.reconciliationController?.abort();
    this.reconciliationController = undefined;
    this.reconciling = false;
    this.targetRequestVersion += 1;
    this.targetController?.abort();
    this.targetController = undefined;
    this.pendingPosts.clear();
    this.needsLiveReconciliation = false;
  }

  private receivePost(value: Post, generation: number) {
    if (this.needsLiveReconciliation || this.postIds.has(value.id) || this.pendingPosts.has(value.id)) return;
    if (this.reconciling) {
      this.reconcileLatest(generation);
      return;
    }
    if (window.scrollY <= 8) {
      this.postIds.add(value.id);
      this.posts.push(value);
      this.posts.sort(comparePosts);
      this.renderFeed();
      void this.loadProvenance(generation, this.controller?.signal ?? new AbortController().signal);
      return;
    }
    if (this.pendingPosts.size >= LIVE_PENDING_LIMIT) {
      this.pendingPosts.clear();
      this.needsLiveReconciliation = true;
      this.showLiveNotice("New content exceeded the live queue; reload the latest posts", true);
      return;
    }
    this.pendingPosts.set(value.id, value);
    this.showLiveNotice(`${this.pendingPosts.size} new ${this.pendingPosts.size === 1 ? "post" : "posts"}`, false);
  }

  private showLiveNotice(message: string, reset: boolean) {
    const existing = this.main?.querySelector<HTMLElement>("[data-live-notice]");
    const existingButton = existing?.querySelector<HTMLButtonElement>("[data-new-posts]");
    if (existing && existingButton) {
      existingButton.textContent = message;
      existingButton.dataset.reset = String(reset);
      return;
    }
    const notice = element("div");
    notice.className = "live-notice";
    notice.dataset.liveNotice = "";
    notice.setAttribute("aria-live", "polite");
    const button = element("button", message);
    button.type = "button";
    button.dataset.newPosts = "";
    button.dataset.reset = String(reset);
    button.addEventListener("click", () => {
      if (button.dataset.reset === "true") {
        this.reconcileLatest(this.connectionGeneration);
        return;
      }
      const pending = [...this.pendingPosts.values()];
      this.pendingPosts.clear();
      for (const post of pending) {
        if (!this.postIds.has(post.id)) { this.postIds.add(post.id); this.posts.push(post); }
      }
      this.posts.sort(comparePosts);
      notice.remove();
      this.renderFeed();
      window.scrollTo({
        top: 0,
        behavior: window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
      });
      const newest = this.main?.querySelector<HTMLElement>("article");
      if (newest) { newest.tabIndex = -1; newest.focus(); }
    });
    notice.append(button);
    this.main?.querySelector(".feed")?.before(notice);
    if (!notice.isConnected) this.main?.append(notice);
  }

  private reconcileLatest(generation: number) {
    if (!this.isAppActive(generation)) return;
    const endpoint = pageEndpoint(this.route);
    if (!endpoint) return;
    this.reconciliationVersion += 1;
    this.reconciliationController?.abort();
    this.targetRequestVersion += 1;
    this.targetController?.abort();
    this.targetController = undefined;
    this.controller?.abort();
    this.controller = new AbortController();
    if (this.reconciling) return;
    void this.runReconciliation(generation, endpoint);
  }

  private async runReconciliation(generation: number, endpoint: string) {
    const version = this.reconciliationVersion;
    const controller = new AbortController();
    this.reconciliationController = controller;
    this.reconciling = true;
    this.main?.querySelectorAll<HTMLButtonElement>("[data-load-more], [data-pagination-retry]")
      .forEach((button) => { button.disabled = true; });
    const oldestLoaded = this.posts.at(-1);
    const reconciled: Post[] = [];
    const reconciledIds = new Set<number>();
    const visitedCursors = new Set<string>();
    let cursor: string | null = null;
    try {
      do {
        if (cursor) {
          if (visitedCursors.has(cursor)) throw new Error("reconciliation cursor made no progress");
          visitedCursors.add(cursor);
        }
        const url = cursor ? `${endpoint}?${new URLSearchParams({ cursor })}` : endpoint;
        const response = await fetch(url, { signal: controller.signal });
        if (!this.isAppActive(generation, controller.signal) || version !== this.reconciliationVersion) return;
        if (response.status === 401) {
          this.showAuthenticationExpired();
          return;
        }
        if (response.status === 404 && this.route.kind === "session") {
          this.showClosed();
          return;
        }
        if (response.status === 404 && this.route.kind === "project") {
          this.posts = [];
          this.postIds.clear();
          this.pendingPosts.clear();
          this.nextCursor = null;
          this.renderFeed();
          return;
        }
        if (!response.ok) throw new Error("reconciliation failed");
        const payload: unknown = await response.json();
        if (!this.isAppActive(generation, controller.signal) || version !== this.reconciliationVersion) return;
        if (!isPage(payload)) throw new Error("malformed reconciliation");
        for (const post of payload.posts) {
          if (!reconciledIds.has(post.id)) {
            reconciledIds.add(post.id);
            reconciled.push(post);
          }
        }
        cursor = payload.next_cursor;
        const fetchedOldest = reconciled.at(-1);
        if (!cursor || !oldestLoaded || (fetchedOldest && comparePosts(fetchedOldest, oldestLoaded) >= 0)) break;
      } while (cursor);
      if (!this.isAppActive(generation, controller.signal) || version !== this.reconciliationVersion) return;
      this.posts = reconciled.sort(comparePosts);
      this.postIds = new Set(this.posts.map((post) => post.id));
      this.nextCursor = cursor;
      this.pendingPosts.clear();
      this.needsLiveReconciliation = false;
      this.main?.querySelector("[data-live-notice]")?.remove();
      this.renderFeed();
      void this.loadProvenance(generation, this.controller?.signal ?? controller.signal);
      void this.resolveLocationPost(generation);
    } catch (error) {
      if (this.isAppActive(generation) && version === this.reconciliationVersion
        && !(error instanceof DOMException && error.name === "AbortError")) {
        this.showLiveNotice("Live updates need a retry", true);
      }
    } finally {
      if (this.reconciliationController !== controller) return;
      this.reconciliationController = undefined;
      this.reconciling = false;
      this.main?.querySelectorAll<HTMLButtonElement>("[data-load-more], [data-pagination-retry]")
        .forEach((button) => { button.disabled = false; });
      if (this.isAppActive(generation) && !this.authenticationExpired && !this.closed
        && version !== this.reconciliationVersion) {
        const currentEndpoint = pageEndpoint(this.route);
        if (currentEndpoint) void this.runReconciliation(generation, currentEndpoint);
      }
    }
  }

  private syncHeartbeat() {
    if (this.route.kind !== "session" || !this.streamOpen || document.visibilityState !== "visible" || this.closed) {
      this.stopHeartbeat();
      return;
    }
    if (this.heartbeatTimer === undefined) {
      void this.sendHeartbeat();
      this.heartbeatTimer = window.setInterval(() => { void this.sendHeartbeat(); }, HEARTBEAT_INTERVAL_MS);
    }
  }

  private async sendHeartbeat() {
    if (this.route.kind !== "session" || this.heartbeatInFlight || !this.streamOpen || document.visibilityState !== "visible") return;
    this.heartbeatInFlight = true;
    const controller = new AbortController();
    this.heartbeatController = controller;
    try { await fetch(`${API}/sessions/${this.route.publicId}/heartbeat`, { method: "POST", signal: controller.signal }); }
    catch { /* Heartbeats never replace a usable feed. */ }
    finally {
      if (this.heartbeatController === controller) {
        this.heartbeatController = undefined;
        this.heartbeatInFlight = false;
      }
    }
  }

  private stopHeartbeat() {
    if (this.heartbeatTimer !== undefined) window.clearInterval(this.heartbeatTimer);
    this.heartbeatTimer = undefined;
    this.heartbeatController?.abort();
    this.heartbeatController = undefined;
    this.heartbeatInFlight = false;
  }

  private showClosed() {
    this.closed = true;
    this.stopLive();
    this.setConnectionStatus("Closed");
    this.controller?.abort();
    this.showState("Session closed");
  }

  private renderLogin(generation: number) {
    const section = element("section");
    section.className = "state";
    const heading = element("h2", "Access token");
    const explanation = element("p", "Enter the token from the configured Glimse token file.");
    const form = element("form");
    const label = element("label", "Access token ");
    const input = element("input");
    input.type = "password";
    input.name = "token";
    input.required = true;
    input.autocomplete = "current-password";
    const submit = element("button", "Sign in");
    submit.type = "submit";
    label.append(input);
    form.append(label, submit);
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      if (submit.disabled || !input.value) return;
      const token = input.value;
      input.value = "";
      submit.disabled = true;
      section.querySelector("[data-login-status]")?.remove();
      const controller = new AbortController();
      this.controller = controller;
      void fetch(`${API}/auth/session`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ token }),
        signal: controller.signal,
      }).then((response) => {
        if (!this.isAppActive(generation, controller.signal)) return;
        if (!response.ok) throw new Error("login rejected");
        window.location.assign("/feed");
      }).catch((error: unknown) => {
        if (!this.isAppActive(generation, controller.signal)
          || (error instanceof DOMException && error.name === "AbortError")) return;
        submit.disabled = false;
        const status = element("p", "Could not sign in. Check the token and try again.");
        status.dataset.loginStatus = "";
        status.setAttribute("role", "status");
        section.append(status);
        input.focus();
      });
    });
    section.append(heading, explanation, form);
    this.main?.append(section);
    input.focus();
  }

  private renderShell() {
    this.shadowRoot?.querySelector("main")?.remove();
    const main = element("main");
    const header = element("header");
    const heading = element("h1", "Glimse");
    const status = element("span", "Offline");
    status.className = "connection-status";
    status.dataset.connectionStatus = "";
    status.setAttribute("role", "status");
    const navigation = element("nav");
    navigation.setAttribute("aria-label", "Feed scopes");
    header.append(heading, status, navigation);
    main.append(header);
    this.shadowRoot!.append(main);
    this.main = main;
    this.navigation = navigation;
    this.connectionStatus = status;
    this.renderNavigation();
  }

  private setConnectionStatus(value: string) {
    if (this.connectionStatus) this.connectionStatus.textContent = value;
  }

  private renderNavigation(context?: Session) {
    if (!this.navigation) return;
    this.navigation.replaceChildren();
    if (this.route.kind === "login") return;
    if (this.route.kind === "session") {
      const sessionLink = element("a", "Session");
      sessionLink.href = `/sessions/${this.route.publicId}`;
      this.navigation.append(sessionLink);
      if (context) {
        const projectLink = element("a", context.project.label || "Project");
        projectLink.href = `/projects/${context.project.id}`;
        this.navigation.append(projectLink);
      }
    } else if (this.route.kind === "project") {
      const projectLink = element("a", context?.project.label || "Project");
      projectLink.href = `/projects/${this.route.projectId}`;
      this.navigation.append(projectLink);
    }
    const globalLink = element("a", "Global feed");
    globalLink.href = "/feed";
    this.navigation.append(globalLink);
    const actions = element("details");
    actions.dataset.actions = "";
    actions.append(element("summary", "Actions"));
    const actionMenu = element("div");
    actionMenu.className = "action-menu";
    const logout = element("button", "Log out");
    logout.type = "button";
    logout.dataset.logout = "";
    logout.addEventListener("click", () => { void this.logout(logout); });
    actionMenu.append(logout);
    if (this.route.kind === "session") {
      const close = element("button", "Close session");
      close.type = "button";
      close.className = "danger";
      close.dataset.closeSession = "";
      close.addEventListener("click", () => { void this.closeSession(close); });
      actionMenu.append(close);
    }
    actions.append(actionMenu);
    this.navigation.append(actions);
  }

  private async logout(button: HTMLButtonElement) {
    if (button.disabled) return;
    button.disabled = true;
    try {
      const response = await fetch(`${API}/auth/session`, { method: "DELETE" });
      if (!response.ok) throw new Error("logout failed");
      this.stopLive();
      this.controller?.abort();
      window.location.assign("/login");
    } catch {
      button.disabled = false;
      const status = element("span", " Could not log out. Try again.");
      status.setAttribute("role", "status");
      button.after(status);
    }
  }

  private async closeSession(button: HTMLButtonElement) {
    if (this.route.kind !== "session" || button.disabled
      || !window.confirm("Close this session and permanently remove its posts?")) return;
    button.disabled = true;
    try {
      const response = await fetch(`${API}/sessions/${this.route.publicId}`, { method: "DELETE" });
      if (!response.ok) throw new Error("close failed");
      this.showClosed();
    } catch {
      button.disabled = false;
      const status = element("span", " Could not close the session. Try again.");
      status.setAttribute("role", "status");
      button.after(status);
    }
  }

  private showAuthenticationExpired() {
    this.authenticationExpired = true;
    this.stopLive();
    this.setConnectionStatus("Sign-in required");
    this.showState("Authentication expired");
    const state = this.main?.querySelector<HTMLElement>(".state");
    if (!state) return;
    const login = element("a", "Sign in");
    login.href = "/login";
    state.append(element("br"), login);
  }

  private showState(message: string, retry = false) {
    this.main?.querySelectorAll(".state, .feed, [data-live-notice], [data-comparison]").forEach((value) => value.remove());
    const state = element("section", message);
    state.className = "state";
    state.setAttribute("aria-live", "polite");
    if (retry) {
      const button = element("button", "Retry");
      button.dataset.retry = "";
      button.addEventListener("click", () => this.load(false).catch(() => undefined));
      state.append(element("br"), button);
    }
    this.main?.append(state);
  }

  private async load(more: boolean, generation = this.connectionGeneration) {
    const endpoint = pageEndpoint(this.route);
    if (!endpoint || !this.isAppActive(generation) || this.reconciling) return;
    if (!more) {
      this.controller?.abort();
      this.controller = new AbortController();
      this.posts = [];
      this.postIds.clear();
      this.nextCursor = null;
      this.sessions.clear();
      this.provenanceUnavailable.clear();
      this.provenanceAttempts.clear();
      this.provenanceInFlight.clear();
      this.showState("Loading feed");
    }
    const controller = this.controller;
    if (!controller || !this.isAppActive(generation, controller.signal)) return;
    const requestedCursor = more ? this.nextCursor : null;
    const url = requestedCursor ? `${endpoint}?${new URLSearchParams({ cursor: requestedCursor })}` : endpoint;
    let response: Response;
    try {
      response = await fetch(url, { signal: controller.signal });
      if (!this.isAppActive(generation, controller.signal)) return;
    } catch (error) {
      if (!this.isAppActive(generation, controller.signal)
        || (error instanceof DOMException && error.name === "AbortError")) return;
      this.handleLoadError(more, "Could not load older posts", "Could not reach the daemon");
      return;
    }
    if (!response.ok) {
      if (response.status === 401) {
        this.showAuthenticationExpired();
        return;
      }
      if (response.status === 404 && this.route.kind === "session") {
        this.showClosed();
        return;
      }
      this.handleLoadError(more, `Could not load older posts (HTTP ${response.status})`, `Feed request failed (HTTP ${response.status})`);
      return;
    }
    let payload: unknown;
    try {
      payload = await response.json();
      if (!this.isAppActive(generation, controller.signal)) return;
    } catch {
      if (!this.isAppActive(generation, controller.signal)) return;
      this.handleLoadError(more, "Could not load older posts because the daemon response was malformed", "The daemon returned a malformed feed response");
      return;
    }
    if (!isPage(payload)) {
      this.handleLoadError(more, "Could not load older posts because the daemon response was malformed", "The daemon returned a malformed feed response");
      return;
    }
    let added = 0;
    for (const candidate of payload.posts) {
      if (!this.postIds.has(candidate.id)) {
        this.postIds.add(candidate.id);
        this.posts.push(candidate);
        added += 1;
      }
    }
    this.posts.sort(comparePosts);
    this.nextCursor = payload.next_cursor;
    this.renderFeed();
    if (more && added === 0 && requestedCursor !== null && payload.next_cursor === requestedCursor) {
      this.nextCursor = null;
      this.showPaginationState("Pagination stopped because the daemon made no progress", false);
    }
    await this.resolveLocationPost(generation);
    await this.loadProvenance(generation, controller.signal);
  }

  private handleLoadError(more: boolean, paginationMessage: string, initialMessage: string) {
    if (more) this.showPaginationState(paginationMessage, true);
    else this.showState(initialMessage, true);
  }

  private showPaginationState(message: string, retry: boolean) {
    const feed = this.main?.querySelector<HTMLElement>(".feed");
    if (!feed) return;
    feed.querySelectorAll("[data-load-more], [data-pagination-state]").forEach((value) => value.remove());
    const state = element("div", message);
    state.dataset.paginationState = "";
    state.setAttribute("aria-live", "polite");
    if (retry) {
      const button = element("button", "Retry loading older posts");
      button.dataset.paginationRetry = "";
      button.addEventListener("click", () => this.load(true).catch(() => undefined));
      state.append(element("br"), button);
    }
    feed.append(state);
  }

  private async resolveLocationPost(generation: number) {
    if (!this.isAppActive(generation)) return;
    this.main?.querySelector("[data-target-state]")?.remove();
    const requestedHash = window.location.hash;
    const comparisonId = comparisonPostId(requestedHash);
    if (comparisonId !== null) {
      if (this.targetController && !this.targetController.signal.aborted) return;
      await this.resolveComparison(comparisonId, requestedHash, generation);
      return;
    }
    const match = requestedHash.match(/^#post-([1-9][0-9]*)$/);
    if (!match) return;
    const postId = Number(match[1]);
    if (!isPositiveSafeInteger(postId)) return;
    const existing = this.main?.querySelector<HTMLElement>(`#post-${postId}`);
    if (existing) {
      if (this.focusedHash !== requestedHash) {
        this.focusedHash = requestedHash;
        this.focusPost(existing);
      }
      return;
    }
    if (this.targetController && !this.targetController.signal.aborted) return;
    const requestVersion = this.targetRequestVersion;
    const controller = new AbortController();
    this.targetController = controller;
    this.targetHash = requestedHash;
    const active = () => this.isAppActive(generation, controller.signal)
      && !this.closed
      && requestVersion === this.targetRequestVersion
      && window.location.hash === requestedHash;
    const aborted = new Promise<never>((_resolve, reject) => {
      controller.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")), { once: true });
    });
    try {
      const response = await Promise.race([fetch(`${API}/posts/${postId}`, { signal: controller.signal }), aborted]);
      if (!active()) return;
      if (response.status === 401) { this.showAuthenticationExpired(); return; }
      if (!response.ok) throw new Error(`Post ${postId} could not be loaded (HTTP ${response.status})`);
      const payload: unknown = await Promise.race([response.json(), aborted]);
      if (!active() || !isPost(payload) || payload.id !== postId) {
        if (active()) throw new Error(`Post ${postId} returned malformed data`);
        return;
      }
      if (this.route.kind === "session" && payload.session_public_id !== this.route.publicId) {
        throw new Error(`Post ${postId} is outside this session`);
      }
      if (this.route.kind === "project") {
        const sessionResponse = await Promise.race([
          fetch(`${API}/sessions/${payload.session_public_id}`, { signal: controller.signal }),
          aborted,
        ]);
        if (!active()) return;
        if (sessionResponse.status === 401) { this.showAuthenticationExpired(); return; }
        const sessionPayload: unknown = sessionResponse.ok
          ? await Promise.race([sessionResponse.json(), aborted])
          : null;
        if (!active()) return;
        if (!isSession(sessionPayload) || sessionPayload.id !== payload.session_id
          || sessionPayload.public_id !== payload.session_public_id
          || sessionPayload.project.id !== this.route.projectId) {
          throw new Error(`Post ${postId} is outside this project`);
        }
        this.sessions.set(sessionPayload.public_id, sessionPayload);
      }
      if (!active()) return;
      if (!this.postIds.has(payload.id)) {
        this.posts.push(payload);
        this.posts.sort(comparePosts);
        this.postIds.add(payload.id);
        this.renderFeed();
      }
      void this.loadProvenance(generation, this.controller?.signal ?? controller.signal);
      const target = this.main?.querySelector<HTMLElement>(`#post-${postId}`);
      if (target && active() && this.focusedHash !== requestedHash) {
        this.focusedHash = requestedHash;
        this.focusPost(target);
      }
    } catch (error) {
      if (!active() || (error instanceof DOMException && error.name === "AbortError")) return;
      const status = element("p", error instanceof Error ? error.message : `Post ${postId} could not be loaded`);
      status.dataset.targetState = "";
      status.className = "target-state";
      status.setAttribute("role", "status");
      this.main?.querySelector(".feed")?.prepend(status);
      if (!status.isConnected) this.main?.append(status);
    } finally {
      if (this.targetController === controller) {
        this.targetController = undefined;
        this.targetHash = undefined;
      }
    }
  }

  private async resolveComparison(postId: number, requestedHash: string, generation: number) {
    const requestVersion = this.targetRequestVersion;
    const controller = new AbortController();
    this.targetController = controller;
    this.targetHash = requestedHash;
    const active = () => this.isAppActive(generation, controller.signal)
      && !this.closed
      && requestVersion === this.targetRequestVersion
      && window.location.hash === requestedHash;
    const fetchPost = async (id: number): Promise<Post | null> => {
      const existing = this.posts.find((post) => post.id === id);
      if (existing) return existing;
      const response = await fetch(`${API}/posts/${id}`, { signal: controller.signal });
      if (!active()) return null;
      if (response.status === 401) { this.showAuthenticationExpired(); return null; }
      if (!response.ok) throw new Error(`Post ${id} could not be loaded (HTTP ${response.status})`);
      const payload: unknown = await response.json();
      if (!active()) return null;
      if (!isPost(payload) || payload.id !== id) throw new Error(`Post ${id} returned malformed data`);
      return payload;
    };
    const validateScope = async (post: Post): Promise<boolean> => {
      if (this.route.kind === "session" && post.session_public_id !== this.route.publicId) {
        throw new Error(`Post ${post.id} is outside this session`);
      }
      if (this.route.kind !== "project") return true;
      let context = this.sessions.get(post.session_public_id);
      if (!context) {
        const response = await fetch(`${API}/sessions/${post.session_public_id}`, { signal: controller.signal });
        if (!active()) return false;
        if (response.status === 401) { this.showAuthenticationExpired(); return false; }
        const payload: unknown = response.ok ? await response.json() : null;
        if (!active()) return false;
        if (!isSession(payload)) throw new Error(`Post ${post.id} is outside this project`);
        context = payload;
      }
      if (context.id !== post.session_id || context.public_id !== post.session_public_id
        || context.project.id !== this.route.projectId) throw new Error(`Post ${post.id} is outside this project`);
      this.sessions.set(context.public_id, context);
      return true;
    };
    try {
      const newer = await fetchPost(postId);
      if (!newer || !active() || !await validateScope(newer) || !active()) return;
      if (newer.predecessor_post_id === null) throw new Error(`Post ${postId} has no predecessor`);
      const older = await fetchPost(newer.predecessor_post_id);
      if (!older || !active()) return;
      if (older.id !== newer.predecessor_post_id
        || older.session_id !== newer.session_id
        || older.session_public_id !== newer.session_public_id) {
        throw new Error("The predecessor relationship is invalid");
      }
      if (active()) this.renderComparison(newer, older);
    } catch (error) {
      if (!active() || (error instanceof DOMException && error.name === "AbortError")) return;
      this.main?.querySelector("[data-comparison]")?.remove();
      const status = element("p", error instanceof Error ? error.message : `Post ${postId} could not be compared`);
      status.dataset.targetState = "";
      status.className = "target-state";
      status.setAttribute("role", "status");
      this.main?.append(status);
    } finally {
      if (this.targetController === controller) {
        this.targetController = undefined;
        this.targetHash = undefined;
      }
    }
  }

  private navigateHash(hash: string) {
    window.history.pushState({}, "", hash);
    this.hashHandler();
  }

  private renderComparison(newer: Post, older: Post) {
    this.main?.querySelectorAll(".state, .feed, [data-comparison]").forEach((value) => value.remove());
    const comparison = element("section");
    comparison.dataset.comparison = "";
    const heading = element("h2", `Comparing post ${newer.id} with post ${older.id}`);
    heading.tabIndex = -1;
    const back = element("a", `Return to post ${newer.id}`);
    back.href = `#post-${newer.id}`;
    back.dataset.returnPost = "";
    back.addEventListener("click", (event) => {
      event.preventDefault();
      this.navigateHash(back.hash);
    });
    comparison.append(heading, back);
    const hasFilename = (file: PostFile) => file.filename.trim().length > 0;
    const comparisonFilename = (file: PostFile) => hasFilename(file) ? file.filename : "Unnamed artifact";
    const oldCounts = new Map<string, number>();
    const newCounts = new Map<string, number>();
    older.files.forEach((file) => oldCounts.set(file.filename, (oldCounts.get(file.filename) ?? 0) + 1));
    newer.files.forEach((file) => newCounts.set(file.filename, (newCounts.get(file.filename) ?? 0) + 1));
    const autoPairs = newer.files.flatMap((newFile) => {
      if (!hasFilename(newFile) || newCounts.get(newFile.filename) !== 1 || oldCounts.get(newFile.filename) !== 1) return [];
      const oldFile = older.files.find((candidate) => candidate.filename === newFile.filename);
      return oldFile ? [{ oldFile, newFile }] : [];
    });
    const added = [...new Set(newer.files.filter((file) => hasFilename(file) && !oldCounts.has(file.filename)).map((file) => file.filename))];
    const removed = [...new Set(older.files.filter((file) => hasFilename(file) && !newCounts.has(file.filename)).map((file) => file.filename))];
    const manual = [...new Set([...older.files, ...newer.files]
      .filter((file) => !hasFilename(file) || (oldCounts.get(file.filename) ?? 0) > 1 || (newCounts.get(file.filename) ?? 0) > 1)
      .map((file) => hasFilename(file) ? file.filename : "unnamed artifacts"))];
    const statusParts = [
      added.length ? `Added: ${added.join(", ")}` : "",
      removed.length ? `Removed: ${removed.join(", ")}` : "",
      manual.length ? `Manual pairing required: ${manual.join(", ")}` : "",
    ].filter(Boolean);
    const status = element("p", statusParts.join(" · ") || "All artifacts paired by unique exact filename");
    status.dataset.pairingStatus = "";
    comparison.append(status);

    const selectors = element("div");
    selectors.className = "comparison-selectors";
    const makeSelect = (labelText: string, files: PostFile[], marker: "oldArtifact" | "newArtifact") => {
      const label = element("label", labelText);
      const select = element("select");
      select.dataset[marker] = "";
      const none = element("option", `No ${labelText.toLowerCase()}`);
      none.value = "";
      select.append(none);
      files.forEach((file) => {
        const option = element("option", `${comparisonFilename(file)} (artifact ${file.position + 1})`);
        option.value = String(file.position);
        select.append(option);
      });
      label.append(select);
      selectors.append(label);
      return select;
    };
    const oldSelect = makeSelect("Previous artifact", older.files, "oldArtifact");
    const newSelect = makeSelect("Current artifact", newer.files, "newArtifact");
    if (autoPairs[0]) {
      oldSelect.value = String(autoPairs[0].oldFile.position);
      newSelect.value = String(autoPairs[0].newFile.position);
    }
    comparison.append(selectors);
    const renderPair = () => {
      comparison.querySelector("[data-selected-pair]")?.remove();
      const oldFile = oldSelect.value === "" ? undefined : older.files[Number(oldSelect.value)];
      const newFile = newSelect.value === "" ? undefined : newer.files[Number(newSelect.value)];
      const selected = element("div");
      selected.dataset.selectedPair = "";
      if (!oldFile || !newFile) {
        const state = element("p", oldFile
          ? `Removed after post ${older.id}: ${comparisonFilename(oldFile)}`
          : newFile
            ? `Added in post ${newer.id}: ${comparisonFilename(newFile)}`
            : "Choose artifacts to compare");
        state.dataset.pairState = "";
        selected.append(state);
      }
      if (oldFile && newFile && ["text", "json"].includes(oldFile.renderer) && ["text", "json"].includes(newFile.renderer)) {
        const textComparison = document.createElement("glim-text-comparison") as GlimTextComparison;
        textComparison.data = {
          older: { postId: older.id, file: oldFile },
          newer: { postId: newer.id, file: newFile },
        };
        selected.append(textComparison);
      } else if (oldFile && newFile && ["image", "svg"].includes(oldFile.renderer) && ["image", "svg"].includes(newFile.renderer)) {
        const images = document.createElement("glim-image-comparison") as GlimImageComparison;
        images.data = {
          older: { postId: older.id, file: oldFile },
          newer: { postId: newer.id, file: newFile },
        };
        selected.append(images);
      } else {
        const columns = element("div");
        columns.className = "comparison-columns";
        for (const [label, post, file, sideName] of [
          ["Previous", older, oldFile, "old"],
          ["Current", newer, newFile, "new"],
        ] as const) {
          if (!file) continue;
          const side = element("section");
          side.dataset.compareSide = sideName;
          side.append(element("h3", `${label}: ${file.filename || "Unnamed artifact"}`));
          const artifact = document.createElement("glim-artifact") as GlimArtifact;
          artifact.data = { postId: post.id, file };
          side.append(artifact);
          columns.append(side);
        }
        selected.append(columns);
      }
      comparison.append(selected);
    };
    oldSelect.addEventListener("change", () => {
      const oldFile = oldSelect.value === "" ? undefined : older.files[Number(oldSelect.value)];
      if (oldFile && hasFilename(oldFile) && oldCounts.get(oldFile.filename) === 1 && newCounts.get(oldFile.filename) === 1) {
        newSelect.value = String(newer.files.find((file) => file.filename === oldFile.filename)!.position);
      }
      renderPair();
    });
    newSelect.addEventListener("change", () => {
      const newFile = newSelect.value === "" ? undefined : newer.files[Number(newSelect.value)];
      if (newFile && hasFilename(newFile) && newCounts.get(newFile.filename) === 1 && oldCounts.get(newFile.filename) === 1) {
        oldSelect.value = String(older.files.find((file) => file.filename === newFile.filename)!.position);
      }
      renderPair();
    });
    renderPair();
    this.main?.append(comparison);
    heading.focus();
  }

  private focusPost(article: HTMLElement) {
    article.tabIndex = -1;
    article.scrollIntoView({
      block: "start",
      behavior: window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
    });
    article.focus({ preventScroll: true });
  }

  private renderFeed() {
    if (comparisonPostId(window.location.hash) !== null) return;
    this.main?.querySelectorAll(".state").forEach((value) => value.remove());
    if (this.posts.length === 0) {
      this.main?.querySelector(".feed")?.remove();
      this.showState("No posts in this feed");
      return;
    }
    let feed = this.main?.querySelector<HTMLElement>(".feed");
    if (!feed) {
      feed = element("section");
      feed.className = "feed";
      feed.setAttribute("aria-label", "Published artifacts");
      this.main?.append(feed);
    }
    feed.querySelectorAll("[data-load-more], [data-pagination-state]").forEach((value) => value.remove());
    const expectedIds = new Set(this.posts.map((post) => `post-${post.id}`));
    const existing = Array.from(feed.querySelectorAll<HTMLElement>("article"));
    const byId = new Map(existing.map((article) => [article.id, article]));
    for (const article of existing) {
      if (!expectedIds.has(article.id)) {
        article.remove();
        byId.delete(article.id);
      }
    }
    let current = feed.firstElementChild;
    for (const post of this.posts) {
      const article = byId.get(`post-${post.id}`) ?? this.renderPost(post);
      while (current && current.tagName !== "ARTICLE") current = current.nextElementSibling;
      if (current === article) current = current.nextElementSibling;
      else feed.insertBefore(article, current);
      byId.set(article.id, article);
    }
    if (this.nextCursor) {
      const button = element("button", "Load older posts");
      button.dataset.loadMore = "";
      button.disabled = this.reconciling;
      button.addEventListener("click", () => {
        button.disabled = true;
        this.load(true).catch(() => undefined);
      });
      feed.append(button);
    }
  }

  private renderPost(post: Post): HTMLElement {
    const article = element("article");
    article.id = `post-${post.id}`;
    const heading = element("h2", post.title);
    const published = new Date(post.published_at * 1000);
    const validPublished = Number.isFinite(published.getTime());
    const time = element("time", validPublished ? published.toLocaleString() : "Publication time unavailable");
    if (validPublished) time.dateTime = published.toISOString();
    time.className = "meta";
    const commentary = element("div");
    commentary.className = "commentary";
    commentary.innerHTML = safeMarkdown(post.commentary);
    article.append(heading, time, commentary);
    if (post.predecessor_post_id !== null) {
      const revision = element("a", `Revises post ${post.predecessor_post_id}`);
      revision.href = `#post-${post.predecessor_post_id}`;
      revision.dataset.revision = "";
      revision.addEventListener("click", (event) => {
        event.preventDefault();
        window.history.pushState({}, "", revision.href);
        this.hashHandler();
      });
      const compare = element("a", "Compare revisions");
      compare.href = `#compare-${post.id}`;
      compare.dataset.compare = "";
      compare.addEventListener("click", (event) => {
        event.preventDefault();
        this.navigateHash(compare.hash);
      });
      article.append(revision, document.createTextNode(" · "), compare);
    }
    const provenanceDetails = element("details");
    provenanceDetails.className = "provenance";
    provenanceDetails.append(element("summary", "Provenance"));
    const provenance = element("p", "Loading provenance");
    provenance.dataset.session = post.session_public_id;
    provenanceDetails.append(provenance);
    if (post.git) {
      const gitParts = [post.git.root, post.git.branch, post.git.commit].filter((value): value is string => !!value);
      provenanceDetails.append(element("p", gitParts.join(" · ")));
    }
    article.append(provenanceDetails);
    const files = element("ol");
    files.className = "files";
    for (const postFile of post.files) {
      const item = element("li");
      const filename = element("div", postFile.filename);
      filename.className = "filename";
      item.append(filename);
      if (postFile.caption !== null) {
        const caption = element("p", postFile.caption);
        caption.className = "caption";
        item.append(caption);
      }
      const artifact = document.createElement("glim-artifact") as GlimArtifact;
      artifact.data = { postId: post.id, file: postFile };
      item.append(artifact);
      files.append(item);
    }
    article.append(files);
    return article;
  }

  private async loadProvenance(generation: number, signal: AbortSignal) {
    const routeSession = this.route.kind === "session" ? [this.route.publicId] : [];
    const ids = [...new Set([...routeSession, ...this.posts.map((post) => post.session_public_id)])];
    await Promise.all(ids.map((id) => this.lookupSession(id, generation, signal)));
    if (!this.isAppActive(generation, signal)) return;
    for (const id of ids) this.renderProvenance(id, this.sessions.get(id) ?? null);
    if (this.route.kind === "session") {
      const context = this.sessions.get(this.route.publicId);
      if (context) this.renderNavigation(context);
    } else if (this.route.kind === "project") {
      const projectId = this.route.projectId;
      const context = [...this.sessions.values()].find((value) => value.project.id === projectId);
      this.renderNavigation(context);
    }
  }

  private lookupSession(id: string, generation: number, signal: AbortSignal, manual = false): Promise<Session | null> {
    const cached = this.sessions.get(id);
    if (cached) return Promise.resolve(cached);
    if (this.provenanceUnavailable.has(id)
      || (!manual && (this.provenanceAttempts.get(id) ?? 0) >= PROVENANCE_RETRY_LIMIT)) return Promise.resolve(null);
    const existing = this.provenanceInFlight.get(id);
    if (existing) return existing;
    let request: Promise<Session | null>;
    request = this.fetchSession(id, generation, signal).finally(() => {
      if (this.provenanceInFlight.get(id) === request) this.provenanceInFlight.delete(id);
    });
    this.provenanceInFlight.set(id, request);
    return request;
  }

  private async fetchSession(id: string, generation: number, signal: AbortSignal): Promise<Session | null> {
    await this.acquireProvenanceSlot();
    try {
      if (!this.isAppActive(generation, signal)) return null;
      const response = await fetch(`${API}/sessions/${id}`, { signal });
      if (!this.isAppActive(generation, signal)) return null;
      if (!response.ok) {
        if (response.status === 401) {
          this.showAuthenticationExpired();
          return null;
        }
        if (response.status >= 500) this.provenanceAttempts.set(id, (this.provenanceAttempts.get(id) ?? 0) + 1);
        else this.provenanceUnavailable.add(id);
        return null;
      }
      const payload: unknown = await response.json();
      if (!this.isAppActive(generation, signal)) return null;
      const expectedSessionId = this.posts.find((post) => post.session_public_id === id)?.session_id;
      const projectMatches = this.route.kind !== "project" || (isSession(payload) && payload.project.id === this.route.projectId);
      if (!isSession(payload) || payload.public_id !== id || !projectMatches
        || (expectedSessionId !== undefined && payload.id !== expectedSessionId)) {
        this.provenanceUnavailable.add(id);
        return null;
      }
      this.sessions.set(id, payload);
      this.provenanceAttempts.delete(id);
      return payload;
    } catch {
      if (!signal.aborted) this.provenanceAttempts.set(id, (this.provenanceAttempts.get(id) ?? 0) + 1);
      return null;
    } finally {
      this.releaseProvenanceSlot();
    }
  }

  private async acquireProvenanceSlot() {
    if (this.provenanceActive < PROVENANCE_CONCURRENCY) {
      this.provenanceActive += 1;
      return;
    }
    await new Promise<void>((resolve) => this.provenanceWaiters.push(resolve));
  }

  private releaseProvenanceSlot() {
    const next = this.provenanceWaiters.shift();
    if (next) next();
    else this.provenanceActive -= 1;
  }

  private renderProvenance(id: string, value: Session | null) {
    const targets = Array.from(this.shadowRoot?.querySelectorAll<HTMLElement>("[data-session]") ?? [])
      .filter((target) => target.dataset.session === id);
    for (const target of targets) {
      if (!value) {
        target.textContent = "Provenance unavailable";
        const attempts = this.provenanceAttempts.get(id) ?? 0;
        if (!this.provenanceUnavailable.has(id) && attempts > 0) {
          const retry = element("button", attempts >= PROVENANCE_RETRY_LIMIT ? "Retry manually" : "Retry");
          retry.type = "button";
          retry.dataset.provenanceRetry = "";
          retry.addEventListener("click", () => {
            retry.disabled = true;
            const signal = this.controller?.signal ?? new AbortController().signal;
            void this.lookupSession(id, this.connectionGeneration, signal, true).then((session) => {
              if (this.isAppActive(this.connectionGeneration, signal)) this.renderProvenance(id, session);
            });
          });
          target.append(document.createTextNode(" · "), retry);
        }
        continue;
      }
      const sessionLink = element("a", "Session");
      sessionLink.href = `/sessions/${value.public_id}`;
      const projectLink = element("a", value.project.label);
      projectLink.href = `/projects/${value.project.id}`;
      target.replaceChildren(
        document.createTextNode(`${value.integration_namespace} · ${value.external_key}\n`),
        sessionLink,
        document.createTextNode(" · "),
        projectLink,
        document.createTextNode(` · ${value.project.working_directory}`),
      );
    }
  }
}

if (!customElements.get("glim-artifact")) customElements.define("glim-artifact", GlimArtifact);
if (!customElements.get("glim-text-comparison")) customElements.define("glim-text-comparison", GlimTextComparison);
if (!customElements.get("glim-image-comparison")) customElements.define("glim-image-comparison", GlimImageComparison);
if (!customElements.get("glim-app")) customElements.define("glim-app", GlimApp);
