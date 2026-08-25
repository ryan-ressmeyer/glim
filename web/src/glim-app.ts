import { marked } from "marked";
import Papa from "papaparse";
import {
  GlobalWorkerOptions,
  getDocument,
  type PDFDocumentLoadingTask,
  type PDFDocumentProxy,
  type PDFPageProxy,
  type RenderTask,
} from "pdfjs-dist";
import PDF_WORKER_URL from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import sanitizeHtml from "sanitize-html";

const API = "/api/v1";
const CSV_MAX_ROWS = 200;
const CSV_MAX_CELLS_PER_ROW = 100;
const PROVENANCE_CONCURRENCY = 4;
const MEDIA_RELEASE_MARGIN = "1000px 0px";
const PDF_LAZY_MARGIN = "1500px 0px";
const PDF_RANGE_CHUNK_BYTES = 64 * 1024;
const PDF_MAX_MATERIALIZED_PAGES = 3;
GlobalWorkerOptions.workerSrc = PDF_WORKER_URL;
const PUBLIC_ID_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const INITIAL_PUBLIC_ID_LENGTH = 6;
const MAX_DATE_SECONDS = 8_640_000_000_000;
// Page-route IDs must remain exactly representable by browser JavaScript.
const MAX_BROWSER_SAFE_ID = 9_007_199_254_740_991;

type Route =
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

interface PdfPageResource {
  canvas: HTMLCanvasElement;
  page: PDFPageProxy;
  task: RenderTask;
}

function ignoreRejection(value: Promise<unknown> | void) {
  void value?.catch(() => undefined);
}

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

function pageEndpoint(route: Route): string | null {
  if (route.kind === "global") return `${API}/posts`;
  if (route.kind === "session") return `${API}/sessions/${route.publicId}/posts`;
  if (route.kind === "project") return `${API}/projects/${route.projectId}/posts`;
  return null;
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

function htmlResourceResolver(postId: number, file: PostFile): (value: string) => string | null {
  const stored = new Set(file.support_assets.map((asset) => asset.relative_path));
  const scope = supportScope(postId, file);
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

function htmlCsp(postId: number, file: PostFile, scripts: boolean): string {
  const scope = new URL(supportScope(postId, file), window.location.href).href;
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

function sanitizeHtmlDocument(source: string, data: ArtifactData, scripts: boolean): string {
  const documentValue = new DOMParser().parseFromString(source, "text/html");
  const resolveSupport = htmlResourceResolver(data.postId, data.file);

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
  csp.setAttribute("content", htmlCsp(data.postId, data.file, scripts));
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
  return Number.isSafeInteger(file.position)
    && (file.position as number) >= 0
    && typeof file.filename === "string"
    && (file.caption === null || typeof file.caption === "string")
    && typeof file.media_type === "string"
    && RENDERERS.has(file.renderer)
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

function isPage(value: unknown): value is Page {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (!Array.isArray(candidate.posts)
    || !(candidate.next_cursor === null || (typeof candidate.next_cursor === "string" && candidate.next_cursor.length > 0))) return false;
  const sessionIds = new Map<string, number>();
  return candidate.posts.every((raw) => {
    if (!raw || typeof raw !== "object") return false;
    const postValue = raw as Record<string, unknown>;
    if (!isPositiveSafeInteger(postValue.id)
      || !isPositiveSafeInteger(postValue.session_id)
      || !isPublicId(postValue.session_public_id)
      || typeof postValue.title !== "string"
      || typeof postValue.commentary !== "string"
      || !isDateSeconds(postValue.published_at)
      || !(postValue.predecessor_post_id === null || isPositiveSafeInteger(postValue.predecessor_post_id))
      || !(postValue.git === null || isGitProvenance(postValue.git))
      || !Array.isArray(postValue.files)
      || !postValue.files.every((file, index) => isPostFile(file) && file.position === index)) return false;
    const previousSessionId = sessionIds.get(postValue.session_public_id);
    if (previousSessionId !== undefined && previousSessionId !== postValue.session_id) return false;
    sessionIds.set(postValue.session_public_id, postValue.session_id);
    return true;
  });
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

const artifactStyles = `
  :host { display: block; }
  img.preview { cursor: zoom-in; display: block; height: auto; max-width: 100%; width: auto; }
  button, a { font: inherit; }
  .preview-control { background: transparent; border: 0; cursor: zoom-in; max-width: 100%; padding: 0; }
  .pane { border: 1px solid #cbd5e1; max-height: 75vh; min-height: 10rem; overflow: auto; resize: vertical; }
  .pane:fullscreen { background: white; max-height: none; padding: 1rem; }
  pre { margin: 0; min-width: max-content; padding: 1rem; white-space: pre; }
  .table-wrap { border: 1px solid #cbd5e1; max-height: 60vh; overflow: auto; }
  table { border-collapse: collapse; }
  th, td { border: 1px solid #cbd5e1; padding: .35rem .55rem; text-align: left; white-space: pre-wrap; }
  .toolbar { display: flex; justify-content: flex-end; margin-bottom: .35rem; }
  .pending, .error { border: 1px dashed #94a3b8; padding: 1rem; }
  .zoom { background: rgb(15 23 42 / 94%); inset: 0; overflow: auto; padding: 4rem; position: fixed; z-index: 1000; }
  .zoom img { display: block; max-width: none; transform-origin: top left; }
  .zoom-controls { left: 1rem; position: fixed; top: 1rem; }
  .zoom-controls button { margin-right: .5rem; }
  .markdown { overflow-wrap: anywhere; }
  .media { display: block; max-height: 75vh; max-width: 100%; width: auto; }
  audio.media { width: min(100%, 40rem); }
  .pdf-document { display: grid; gap: 1rem; overflow: visible; width: 100%; }
  .pdf-page { align-items: start; background: #f1f5f9; display: grid; justify-items: center; min-height: 8rem; width: 100%; }
  .pdf-page canvas { display: block; height: auto; max-width: 100%; width: 100%; }
  .pdf-page button { margin: 2rem; }
  .html-frame { border: 1px solid #cbd5e1; display: block; height: min(60vh, 42rem); max-height: 75vh; min-height: 18rem; width: 100%; }
  .script-warning { background: #fff7ed; border: 1px solid #fdba74; margin-bottom: .75rem; padding: .75rem; }
  .script-warning button { display: block; margin-top: .5rem; }
  .download { display: inline-block; margin-top: .5rem; }
`;

class GlimArtifact extends HTMLElement {
  data?: ArtifactData;
  private controller?: AbortController;
  private closeZoom?: (restoreFocus?: boolean) => void;
  private mediaObserver?: IntersectionObserver;
  private pdfObserver?: IntersectionObserver;
  private pdfLoadingTask?: PDFDocumentLoadingTask;
  private pdfDocument?: PDFDocumentProxy;
  private pdfPages = new Map<number, PdfPageResource>();
  private pdfPageOrder: number[] = [];
  private pdfPending = new Set<number>();
  private pdfVisible = new Set<number>();
  private renderGeneration = 0;

  connectedCallback() {
    const generation = ++this.renderGeneration;
    this.controller?.abort();
    this.releaseRichResources();
    this.destroyHtmlContexts();
    this.removeImageSources();
    this.closeZoom?.(false);
    const root = this.root();
    root.replaceChildren(root.querySelector("style")!);
    this.render(generation).catch((error: unknown) => {
      if (this.isRenderActive(generation)
        && !(error instanceof DOMException && error.name === "AbortError")) this.renderFailure();
    });
  }

  disconnectedCallback() {
    this.renderGeneration += 1;
    this.controller?.abort();
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

  private async fetchText(data: ArtifactData, generation: number): Promise<string | null> {
    const controller = new AbortController();
    this.controller = controller;
    const response = await fetch(artifactUrl(data.postId, data.file.position), { signal: controller.signal });
    if (!this.isRenderActive(generation, controller.signal)) return null;
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const text = await response.text();
    if (!this.isRenderActive(generation, controller.signal)) return null;
    return text;
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
        await this.renderPdf(root, data, generation);
        return;
      case "markdown": {
        const text = await this.fetchText(data, generation);
        if (text === null || !this.isRenderActive(generation)) return;
        const container = element("div");
        container.className = "markdown";
        container.innerHTML = safeMarkdown(text, supportResolver(data.postId, data.file));
        root.append(container);
        return;
      }
      case "text": {
        const text = await this.fetchText(data, generation);
        if (text !== null && this.isRenderActive(generation)) this.renderPane(root, text, data);
        return;
      }
      case "json": {
        const text = await this.fetchText(data, generation);
        if (text === null || !this.isRenderActive(generation)) return;
        try {
          this.renderPane(root, JSON.stringify(JSON.parse(text), null, 2), data);
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
        if (text !== null && this.isRenderActive(generation)) this.renderHtml(root, text, data, generation);
        return;
      }
      case "download":
        root.append(downloadLink(data));
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

  private async renderPdf(root: ShadowRoot, data: ArtifactData, generation: number) {
    GlobalWorkerOptions.workerSrc = PDF_WORKER_URL;
    const options = {
      url: artifactUrl(data.postId, data.file.position),
      rangeChunkSize: PDF_RANGE_CHUNK_BYTES,
      isEvalSupported: false,
    };
    const loadingTask = getDocument(options as Parameters<typeof getDocument>[0]);
    this.pdfLoadingTask = loadingTask;
    const documentProxy = await loadingTask.promise;
    if (!this.isRenderActive(generation) || this.pdfLoadingTask !== loadingTask) {
      ignoreRejection(documentProxy.cleanup());
      return;
    }
    this.pdfDocument = documentProxy;
    const container = element("div");
    container.className = "pdf-document";
    const placeholders: HTMLElement[] = [];
    for (let number = 1; number <= documentProxy.numPages; number += 1) {
      const placeholder = element("div", `Page ${number}`);
      placeholder.className = "pdf-page";
      placeholder.dataset.page = String(number);
      placeholders.push(placeholder);
      container.append(placeholder);
    }
    root.append(container, downloadLink(data));

    const materialize = (placeholder: HTMLElement) => {
      const number = Number(placeholder.dataset.page);
      void this.materializePdfPage(placeholder, number, generation).catch(() => {
        if (this.isRenderActive(generation)) this.renderFailure();
      });
    };
    if (typeof IntersectionObserver === "undefined") {
      if (placeholders[0]) {
        this.pdfVisible.add(1);
        materialize(placeholders[0]);
      }
      return;
    }
    this.pdfObserver = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        const placeholder = entry.target as HTMLElement;
        const number = Number(placeholder.dataset.page);
        if (entry.isIntersecting) {
          this.pdfVisible.add(number);
          materialize(placeholder);
        } else {
          this.pdfVisible.delete(number);
          this.releasePdfPage(number);
        }
      }
    }, { rootMargin: PDF_LAZY_MARGIN });
    placeholders.forEach((placeholder) => this.pdfObserver?.observe(placeholder));
  }

  private async materializePdfPage(placeholder: HTMLElement, number: number, generation: number) {
    if (!this.pdfDocument || this.pdfPending.has(number) || this.pdfPages.has(number)
      || !this.isRenderActive(generation)) return;
    this.pdfPending.add(number);
    let page: PDFPageProxy | undefined;
    try {
      page = await this.pdfDocument.getPage(number);
      if (!this.isRenderActive(generation) || !placeholder.isConnected || !this.pdfVisible.has(number)) {
        page.cleanup();
        return;
      }
      const natural = page.getViewport({ scale: 1 });
      const width = placeholder.clientWidth || placeholder.getBoundingClientRect().width || this.clientWidth || natural.width;
      const viewport = page.getViewport({ scale: width / natural.width });
      const pixelRatio = window.devicePixelRatio || 1;
      const canvas = element("canvas");
      canvas.width = Math.ceil(viewport.width * pixelRatio);
      canvas.height = Math.ceil(viewport.height * pixelRatio);
      const context = canvas.getContext("2d");
      if (!context) throw new Error("Canvas rendering is unavailable");
      const task = page.render({
        canvas,
        canvasContext: context,
        viewport,
        transform: pixelRatio === 1 ? undefined : [pixelRatio, 0, 0, pixelRatio, 0, 0],
      });
      const resource = { canvas, page, task };
      this.pdfPages.set(number, resource);
      this.touchPdfPage(number);
      this.enforcePdfPageLimit(number);
      try {
        await task.promise;
      } catch (error) {
        const shouldReport = this.isRenderActive(generation) && this.pdfPages.get(number) === resource;
        if (this.pdfPages.get(number) === resource) this.releasePdfPage(number);
        if (shouldReport) throw error;
        return;
      }
      if (!this.isRenderActive(generation) || this.pdfPages.get(number) !== resource || !placeholder.isConnected) return;
      placeholder.replaceChildren(canvas);
    } finally {
      this.pdfPending.delete(number);
      if (page && !this.isRenderActive(generation) && !this.pdfPages.has(number)) page.cleanup();
    }
  }

  private touchPdfPage(number: number) {
    this.pdfPageOrder = this.pdfPageOrder.filter((candidate) => candidate !== number);
    this.pdfPageOrder.push(number);
  }

  private enforcePdfPageLimit(preserve: number) {
    while (this.pdfPages.size > PDF_MAX_MATERIALIZED_PAGES) {
      const oldest = this.pdfPageOrder.find((number) => number !== preserve);
      if (oldest === undefined) return;
      this.releasePdfPage(oldest);
    }
  }

  private releasePdfPage(number: number) {
    const resource = this.pdfPages.get(number);
    if (!resource) return;
    this.pdfPages.delete(number);
    this.pdfPageOrder = this.pdfPageOrder.filter((candidate) => candidate !== number);
    try { resource.task.cancel(); } catch { /* already settled */ }
    try { resource.page.cleanup(); } catch { /* already released */ }
    resource.canvas.remove();
    const placeholder = this.shadowRoot?.querySelector<HTMLElement>(`.pdf-page[data-page="${number}"]`);
    if (placeholder && !placeholder.textContent) placeholder.textContent = `Page ${number}`;
  }

  private releaseRichResources() {
    this.mediaObserver?.disconnect();
    this.mediaObserver = undefined;
    this.shadowRoot?.querySelectorAll<HTMLMediaElement>("video, audio").forEach((media) => {
      media.pause();
      media.removeAttribute("src");
      media.load();
    });
    this.pdfObserver?.disconnect();
    this.pdfObserver = undefined;
    for (const number of [...this.pdfPages.keys()]) this.releasePdfPage(number);
    this.pdfPending.clear();
    this.pdfVisible.clear();
    this.pdfPageOrder = [];
    const loadingTask = this.pdfLoadingTask;
    const documentProxy = this.pdfDocument;
    this.pdfLoadingTask = undefined;
    this.pdfDocument = undefined;
    if (loadingTask) ignoreRejection(loadingTask.destroy());
    if (documentProxy) ignoreRejection(documentProxy.cleanup());
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
    const dialog = element("div");
    dialog.className = "zoom";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    dialog.setAttribute("aria-label", `Full-resolution view of ${data.file.filename}`);
    dialog.tabIndex = -1;
    const image = element("img");
    image.src = artifactUrl(data.postId, data.file.position);
    image.alt = data.file.caption ?? data.file.filename;
    let scale = 1;
    const controls = element("div");
    controls.className = "zoom-controls";
    const close = element("button", "Close");
    const zoomIn = element("button", "Zoom in");
    const zoomOut = element("button", "Zoom out");
    const applyScale = () => { image.style.transform = `scale(${scale})`; };
    zoomIn.addEventListener("click", () => { scale = Math.min(4, scale + 0.25); applyScale(); });
    zoomOut.addEventListener("click", () => { scale = Math.max(0.25, scale - 0.25); applyScale(); });
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") this.closeZoom?.();
      if (event.key === "+") zoomIn.click();
      if (event.key === "-") zoomOut.click();
    };
    this.closeZoom = (restoreFocus = true) => {
      window.removeEventListener("keydown", onKey);
      image.removeAttribute("src");
      dialog.remove();
      this.closeZoom = undefined;
      if (restoreFocus && trigger.isConnected) trigger.focus();
    };
    close.addEventListener("click", () => this.closeZoom?.());
    controls.append(close, zoomIn, zoomOut);
    dialog.append(controls, image);
    root.append(dialog);
    window.addEventListener("keydown", onKey);
    dialog.focus();
  }

  private renderPane(root: ShadowRoot, text: string, data: ArtifactData) {
    const toolbar = element("div");
    toolbar.className = "toolbar";
    const fullscreen = element("button", "Fullscreen");
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
    root.append(toolbar, pane, downloadLink(data));
  }

  private renderCsv(root: ShadowRoot, text: string, data: ArtifactData) {
    const parsed = Papa.parse<string[]>(text, { skipEmptyLines: false });
    const rows = parsed.data.slice(0, CSV_MAX_ROWS);
    const wrapper = element("div");
    wrapper.className = "table-wrap";
    wrapper.tabIndex = 0;
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
    root.append(wrapper);
    if (parsed.data.length > CSV_MAX_ROWS) root.append(element("p", `Showing the first ${CSV_MAX_ROWS} rows`));
    root.append(downloadLink(data));
  }

  private renderHtml(root: ShadowRoot, source: string, data: ArtifactData, generation: number) {
    const warning = element("div");
    warning.className = "script-warning";
    warning.dataset.scriptWarning = "";
    warning.append(document.createTextNode(
      "Scripts are disabled. Enabling scripts lets this document navigate its own frame and thereby make a network request.",
    ));
    const enable = element("button", "Enable sandboxed scripts");
    enable.type = "button";
    enable.dataset.enableScripts = "";
    warning.append(enable);

    const createFrame = (scripts: boolean) => {
      const frame = element("iframe");
      frame.className = "html-frame";
      frame.title = `Rendered HTML: ${data.file.filename}`;
      frame.referrerPolicy = "no-referrer";
      frame.setAttribute("sandbox", scripts ? "allow-scripts" : "");
      frame.srcdoc = sanitizeHtmlDocument(source, data, scripts);
      return frame;
    };
    let frame = createFrame(false);
    enable.addEventListener("click", () => {
      if (!this.isRenderActive(generation) || enable.disabled) return;
      enable.disabled = true;
      const replacement = createFrame(true);
      frame.srcdoc = "";
      frame.removeAttribute("src");
      frame.replaceWith(replacement);
      frame = replacement;
    });
    root.append(warning, frame, downloadLink(data));
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
    const error = element("div", `Could not render ${data.file.filename}`);
    error.className = "error";
    error.append(downloadLink(data));
    root.append(error);
  }
}

const appStyles = `
  :host { color: #172033; display: block; font-family: Inter, ui-sans-serif, system-ui, sans-serif; }
  * { box-sizing: border-box; }
  main { margin: 0 auto; max-width: 72rem; padding: 1.5rem; }
  header { align-items: baseline; display: flex; flex-wrap: wrap; gap: 1rem; justify-content: space-between; margin-bottom: 2rem; }
  h1 { font-size: clamp(1.7rem, 4vw, 2.6rem); margin: 0; }
  nav { display: flex; flex-wrap: wrap; gap: .8rem; }
  a { color: #2456a6; }
  article { background: #fff; border: 1px solid #d8dee9; border-radius: .75rem; margin-bottom: 2rem; padding: clamp(1rem, 3vw, 2rem); }
  article h2 { margin-top: 0; }
  .commentary { line-height: 1.6; overflow-wrap: anywhere; }
  .meta, .provenance { color: #526077; font-size: .9rem; }
  .files { list-style: none; margin: 1.5rem 0 0; padding: 0; }
  .files > li { border-top: 1px solid #e5e9f0; padding: 1.25rem 0; }
  .filename { font-weight: 650; }
  .caption { margin: .35rem 0 .75rem; white-space: pre-wrap; }
  .state { border: 1px solid #d8dee9; border-radius: .75rem; padding: 2rem; text-align: center; }
  button { font: inherit; padding: .45rem .8rem; }
  @media (max-width: 36rem) { main { padding: .75rem; } article { border-radius: 0; margin-inline: -.75rem; } }
`;

class GlimApp extends HTMLElement {
  private route: Route = { kind: "invalid" };
  private posts: Post[] = [];
  private postIds = new Set<number>();
  private nextCursor: string | null = null;
  private sessions = new Map<string, Session>();
  private provenanceUnavailable = new Set<string>();
  private provenanceInFlight = new Map<string, Promise<Session | null>>();
  private provenanceActive = 0;
  private provenanceWaiters: Array<() => void> = [];
  private controller?: AbortController;
  private main?: HTMLElement;
  private navigation?: HTMLElement;
  private connectionGeneration = 0;

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
    this.route = routeFromLocation(window.location.pathname);
    this.renderShell();
    if (this.route.kind === "invalid") {
      this.showState("Page not found");
      return;
    }
    this.load(false, generation).catch(() => undefined);
  }

  disconnectedCallback() {
    this.connectionGeneration += 1;
    this.controller?.abort();
  }

  private isAppActive(generation: number, signal?: AbortSignal): boolean {
    return generation === this.connectionGeneration && this.isConnected && !signal?.aborted;
  }

  private renderShell() {
    this.shadowRoot?.querySelector("main")?.remove();
    const main = element("main");
    const header = element("header");
    const heading = element("h1", "Glimse");
    const navigation = element("nav");
    navigation.setAttribute("aria-label", "Feed scopes");
    header.append(heading, navigation);
    main.append(header);
    this.shadowRoot!.append(main);
    this.main = main;
    this.navigation = navigation;
    this.renderNavigation();
  }

  private renderNavigation(context?: Session) {
    if (!this.navigation) return;
    this.navigation.replaceChildren();
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
  }

  private showState(message: string, retry = false) {
    this.main?.querySelectorAll(".state, .feed").forEach((value) => value.remove());
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
    if (!endpoint || !this.isAppActive(generation)) return;
    if (!more) {
      this.controller?.abort();
      this.controller = new AbortController();
      this.posts = [];
      this.postIds.clear();
      this.nextCursor = null;
      this.sessions.clear();
      this.provenanceUnavailable.clear();
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
    this.nextCursor = payload.next_cursor;
    this.renderFeed();
    if (more && added === 0 && requestedCursor !== null && payload.next_cursor === requestedCursor) {
      this.nextCursor = null;
      this.showPaginationState("Pagination stopped because the daemon made no progress", false);
    }
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

  private renderFeed() {
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
    for (const post of this.posts) {
      if (!feed.querySelector(`#post-${post.id}`)) feed.append(this.renderPost(post));
    }
    if (this.nextCursor) {
      const button = element("button", "Load older posts");
      button.dataset.loadMore = "";
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
      article.append(revision);
    }
    const provenance = element("p", "Loading provenance");
    provenance.className = "provenance";
    provenance.dataset.session = post.session_public_id;
    article.append(provenance);
    if (post.git) {
      const gitParts = [post.git.root, post.git.branch, post.git.commit].filter((value): value is string => !!value);
      const git = element("p", gitParts.join(" · "));
      git.className = "provenance";
      article.append(git);
    }
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

  private lookupSession(id: string, generation: number, signal: AbortSignal): Promise<Session | null> {
    const cached = this.sessions.get(id);
    if (cached) return Promise.resolve(cached);
    if (this.provenanceUnavailable.has(id)) return Promise.resolve(null);
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
        this.provenanceUnavailable.add(id);
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
      return payload;
    } catch {
      if (!signal.aborted) this.provenanceUnavailable.add(id);
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
if (!customElements.get("glim-app")) customElements.define("glim-app", GlimApp);
