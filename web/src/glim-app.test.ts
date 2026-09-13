import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import "./glim-app";

type Renderer =
  | "image"
  | "svg"
  | "pdf"
  | "video"
  | "audio"
  | "markdown"
  | "text"
  | "json"
  | "csv"
  | "html"
  | "download";

const session = {
  id: 7,
  public_id: "2zY8Ab",
  integration_namespace: "pi",
  external_key: "agent-session",
  project: { id: 42, label: "Vision study", working_directory: "/work/vision" },
  created_at: 1,
  last_activity_at: 2,
};

function file(position: number, renderer: Renderer, filename = `${renderer}.dat`, caption: string | null = null) {
  return {
    position,
    filename,
    caption,
    media_type: "application/octet-stream",
    renderer,
    blob: { hash: "never-render-this-hash", byte_size: 12 },
    support_assets: [],
  };
}

function post(id: number, overrides: Record<string, unknown> = {}) {
  return {
    id,
    session_id: 7,
    session_public_id: "2zY8Ab",
    title: `Post ${id}`,
    commentary: "First line\n\nSecond line",
    predecessor_post_id: null,
    published_at: 1_725_000_000 + id,
    git: null,
    files: [file(0, "download", "result.bin")],
    ...overrides,
  };
}

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function setPath(path: string) {
  window.history.replaceState({}, "", path);
}

function mount(): HTMLElement {
  const element = document.createElement("glim-app");
  document.body.append(element);
  return element;
}

function composedText(element: HTMLElement): string {
  const rootText = element.shadowRoot?.textContent ?? "";
  const artifactText = Array.from(element.shadowRoot?.querySelectorAll<HTMLElement>("glim-artifact") ?? [])
    .map((artifact) => artifact.shadowRoot?.textContent ?? "")
    .join(" ");
  return `${rootText} ${artifactText}`;
}

async function rendered(element: HTMLElement, text: string) {
  await vi.waitFor(() => expect(composedText(element)).toContain(text));
}

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;
  readonly CONNECTING = 0;
  readonly OPEN = 1;
  readonly CLOSED = 2;
  readyState = FakeEventSource.CONNECTING;
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  private listeners = new Map<string, Array<(event: MessageEvent) => void>>();

  constructor(readonly url: string) { FakeEventSource.instances.push(this); }
  addEventListener(type: string, listener: EventListener) {
    const values = this.listeners.get(type) ?? [];
    values.push(listener as (event: MessageEvent) => void);
    this.listeners.set(type, values);
  }
  close() { this.readyState = FakeEventSource.CLOSED; }
  open() { this.readyState = FakeEventSource.OPEN; this.onopen?.(new Event("open")); }
  error() { this.readyState = FakeEventSource.CONNECTING; this.onerror?.(new Event("error")); }
  emit(type: string, data: unknown, lastEventId = "") {
    const event = new MessageEvent(type, { data: JSON.stringify(data), lastEventId });
    this.listeners.get(type)?.forEach((listener) => listener(event));
  }
}

class TestIntersectionObserver {
  static instances: TestIntersectionObserver[] = [];
  readonly observed = new Set<Element>();
  readonly options: IntersectionObserverInit | undefined;

  constructor(
    private readonly callback: IntersectionObserverCallback,
    options?: IntersectionObserverInit,
  ) {
    this.options = options;
    TestIntersectionObserver.instances.push(this);
  }

  observe(target: Element) { this.observed.add(target); }
  unobserve(target: Element) { this.observed.delete(target); }
  disconnect() { this.observed.clear(); }
  takeRecords(): IntersectionObserverEntry[] { return []; }
  trigger(target: Element, isIntersecting: boolean) {
    this.callback([{ target, isIntersecting } as IntersectionObserverEntry], this as unknown as IntersectionObserver);
  }
}

describe("glim-app public route and element behavior", () => {
  beforeEach(() => {
    setPath("/feed");
    TestIntersectionObserver.instances = [];
    FakeEventSource.instances = [];
    vi.stubGlobal("EventSource", FakeEventSource);
    vi.stubGlobal("IntersectionObserver", undefined);
  });

  afterEach(() => {
    document.body.replaceChildren();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  test.each([
    ["/sessions/2zY8Ab", "/api/v1/sessions/2zY8Ab/posts"],
    ["/projects/42", "/api/v1/projects/42/posts"],
    ["/feed", "/api/v1/posts"],
    ["/", "/api/v1/posts"],
  ])("selects the feed endpoint for %s", async (path, endpoint) => {
    setPath(path);
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === endpoint) return jsonResponse({ posts: [], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    await rendered(element, "No posts in this feed");

    expect(fetchMock).toHaveBeenCalledWith(endpoint, expect.objectContaining({ signal: expect.any(AbortSignal) }));
  });

  test.each([
    "/sessions",
    "/sessions/abc",
    "/sessions/0OIlxx",
    "/sessions/not-valid!",
    "/projects/0",
    "/projects/9007199254740992",
    "/projects/1/extra",
    "/unknown",
  ])(
    "rejects malformed page route %s without fetching",
    async (path) => {
      setPath(path);
      const fetchMock = vi.fn();
      vi.stubGlobal("fetch", fetchMock);

      const element = mount();
      await rendered(element, "Page not found");

      expect(fetchMock).not.toHaveBeenCalled();
    },
  );

  test("loads session context for navigation even when the session feed is empty", async () => {
    setPath("/sessions/2zY8Ab");
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/sessions/2zY8Ab/posts") return jsonResponse({ posts: [], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    await vi.waitFor(() => expect(element.shadowRoot?.querySelector('a[href="/projects/42"]')).not.toBeNull());
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sessions/2zY8Ab", expect.objectContaining({ signal: expect.any(AbortSignal) }));
  });

  test("keeps API order and deduplicates posts across bounded pagination", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") {
        return jsonResponse({ posts: [post(3, { files: [file(0, "text", "once.txt")] }), post(2)], next_cursor: "next page" });
      }
      if (url === "/api/v1/posts?cursor=next+page") {
        return jsonResponse({ posts: [post(2), post(1)], next_cursor: null });
      }
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url === "/api/v1/posts/3/files/0/content") return new Response("fetched once");
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    await rendered(element, "fetched once");
    const loadMore = element.shadowRoot?.querySelector<HTMLButtonElement>("[data-load-more]");
    loadMore?.click();
    await rendered(element, "Post 1");

    const posts = Array.from(element.shadowRoot?.querySelectorAll("article") ?? []);
    expect(posts.map((value) => value.id)).toEqual(["post-3", "post-2", "post-1"]);
    expect(fetchMock.mock.calls.filter(([input]) => String(input).endsWith("/posts/3/files/0/content"))).toHaveLength(1);
    expect(fetchMock.mock.calls.filter(([input]) => String(input) === "/api/v1/sessions/2zY8Ab")).toHaveLength(1);
  });

  test("presents post markdown, captions, revision, Git, provenance, and scoped navigation safely", async () => {
    const richPost = post(10, {
      title: "Retinal response",
      commentary: "**Focused**\n\n<img src=x onerror=alert(1)> [unsafe](javascript:alert(1))",
      predecessor_post_id: 9,
      git: { root: "/work/vision", branch: "main", commit: "abcdef0123456789abcdef0123456789abcdef01" },
      files: [file(0, "download", "response.bin", "Mean response")],
    });
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [richPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    await rendered(element, "agent-session");
    const root = element.shadowRoot!;

    expect(root.querySelector("article")?.id).toBe("post-10");
    expect(root.querySelector("article strong")?.textContent).toBe("Focused");
    expect(root.querySelector("article")?.innerHTML).not.toContain("onerror");
    expect(root.querySelector("article")?.innerHTML).not.toContain("javascript:");
    expect(root.textContent).toContain("Mean response");
    expect(root.querySelector<HTMLAnchorElement>("[data-revision]")?.href).toContain("#post-9");
    expect(root.textContent).toContain("pi · agent-session");
    expect(root.textContent).toContain("Vision study · /work/vision");
    expect(root.textContent).toContain("main · abcdef0123456789abcdef0123456789abcdef01");
    expect(root.querySelector<HTMLAnchorElement>('a[href="/sessions/2zY8Ab"]')).not.toBeNull();
    expect(root.querySelector<HTMLAnchorElement>('a[href="/projects/42"]')).not.toBeNull();
    expect(root.querySelector<HTMLAnchorElement>('a[href="/feed"]')).not.toBeNull();
    expect(root.textContent).not.toContain("never-render-this-hash");
  });

  test("opens an immediate-predecessor comparison without letting live rendering replace it and returns focus to the post", async () => {
    const newer = post(10, { predecessor_post_id: 9, files: [file(0, "download", "result.bin")] });
    const older = post(9, { files: [file(0, "download", "result.bin")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");

    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();

    await rendered(app, "Comparing post 10 with post 9");
    expect(window.location.hash).toBe("#compare-10");
    expect(app.shadowRoot?.querySelector("[data-comparison]")).not.toBeNull();
    expect(app.shadowRoot?.querySelector(".feed")).toBeNull();
    expect(app.shadowRoot?.querySelectorAll("[data-comparison] glim-artifact")).toHaveLength(2);
    FakeEventSource.instances[0].emit("post", post(11));
    expect(app.shadowRoot?.querySelector("[data-comparison]")).not.toBeNull();
    expect(app.shadowRoot?.querySelector(".feed")).toBeNull();

    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-return-post]")?.click();
    await vi.waitFor(() => expect(app.shadowRoot?.activeElement?.id).toBe("post-10"));
    expect(window.location.hash).toBe("#post-10");
  });

  test("restores a cold scoped comparison by fetching and validating the revised post", async () => {
    setPath("/sessions/2zY8Ab#compare-10");
    const newer = post(10, { predecessor_post_id: 9 });
    const older = post(9);
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/sessions/2zY8Ab/posts") return jsonResponse({ posts: [post(11)], next_cursor: null });
      if (url === "/api/v1/posts/10") return jsonResponse(newer);
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));

    const app = mount();

    await rendered(app, "Comparing post 10 with post 9");
    expect(app.shadowRoot?.querySelector("#post-10")).toBeNull();
    expect(app.shadowRoot?.querySelector("[data-comparison]")).not.toBeNull();
  });

  test("pairs only unique nonblank exact filenames and supports manual renamed or duplicate selection", async () => {
    const newer = post(10, {
      predecessor_post_id: 9,
      files: [
        file(0, "download", "duplicate.bin"),
        file(1, "download", "stable.bin"),
        file(2, "download", "duplicate.bin"),
        file(3, "download", "added.bin"),
        file(4, "download", ""),
        file(5, "download", "second-stable.bin"),
      ],
    });
    const older = post(9, {
      files: [
        file(0, "download", "stable.bin"),
        file(1, "download", "duplicate.bin"),
        file(2, "download", "duplicate.bin"),
        file(3, "download", "removed.bin"),
        file(4, "download", ""),
        file(5, "download", "second-stable.bin"),
      ],
    });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();
    await rendered(app, "Comparing post 10 with post 9");

    const oldSelect = app.shadowRoot?.querySelector<HTMLSelectElement>("[data-old-artifact]")!;
    const newSelect = app.shadowRoot?.querySelector<HTMLSelectElement>("[data-new-artifact]")!;
    expect(oldSelect.value).toBe("0");
    expect(newSelect.value).toBe("1");
    expect(app.shadowRoot?.querySelector("[data-pairing-status]")?.textContent).toContain("Added: added.bin");
    expect(app.shadowRoot?.querySelector("[data-pairing-status]")?.textContent).toContain("Removed: removed.bin");
    expect(app.shadowRoot?.querySelector("[data-pairing-status]")?.textContent).toContain("Manual pairing required: duplicate.bin, unnamed artifacts");
    newSelect.value = "5";
    newSelect.dispatchEvent(new Event("change"));
    expect(oldSelect.value).toBe("5");

    oldSelect.value = "1";
    oldSelect.dispatchEvent(new Event("change"));
    newSelect.value = "0";
    newSelect.dispatchEvent(new Event("change"));
    const links = Array.from(app.shadowRoot?.querySelectorAll<HTMLElement>("[data-selected-pair] glim-artifact") ?? [])
      .map((artifact) => artifact.shadowRoot?.querySelector<HTMLAnchorElement>("a[download]")?.getAttribute("href"));
    expect(links).toEqual([
      "/api/v1/posts/9/files/1/content",
      "/api/v1/posts/10/files/0/content",
    ]);

    oldSelect.value = "";
    oldSelect.dispatchEvent(new Event("change"));
    newSelect.value = "3";
    newSelect.dispatchEvent(new Event("change"));
    expect(app.shadowRoot?.querySelector("[data-pair-state]")?.textContent).toBe("Added in post 10: added.bin");
    expect(app.shadowRoot?.querySelectorAll("[data-selected-pair] glim-artifact")).toHaveLength(1);
  });

  test("requires whitespace-only filenames to be paired manually", async () => {
    const newer = post(10, { predecessor_post_id: 9, files: [file(0, "download", "   ")] });
    const older = post(9, { files: [file(0, "download", "   ")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();
    await rendered(app, "Comparing post 10 with post 9");

    expect(app.shadowRoot?.querySelector<HTMLSelectElement>("[data-old-artifact]")?.value).toBe("");
    expect(app.shadowRoot?.querySelector<HTMLSelectElement>("[data-new-artifact]")?.value).toBe("");
    expect(app.shadowRoot?.querySelector("[data-pairing-status]")?.textContent).toContain("Manual pairing required: unnamed artifacts");
  });

  test("renders image revisions in independent panes with shared percentage zoom and Fit both", async () => {
    const newer = post(10, { predecessor_post_id: 9, files: [file(0, "image", "plot.png", "Current plot")] });
    const older = post(9, { files: [file(0, "image", "plot.png", "Previous plot")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();
    await rendered(app, "Comparing post 10 with post 9");

    const imageComparison = app.shadowRoot?.querySelector<HTMLElement>("[data-image-comparison]")!;
    const images = Array.from(imageComparison.querySelectorAll<HTMLImageElement>("[data-compare-image]"));
    const panes = Array.from(imageComparison.querySelectorAll<HTMLElement>("[data-image-pane]"));
    expect(images).toHaveLength(2);
    expect(panes).toHaveLength(2);
    expect(imageComparison.querySelector("[data-compare-actual]")).not.toBeNull();
    expect(imageComparison.querySelector("[data-compare-zoom-out]")).not.toBeNull();
    expect(Array.from(imageComparison.querySelectorAll<HTMLAnchorElement>("a[download]")).map((link) => link.getAttribute("href"))).toEqual([
      "/api/v1/posts/9/files/0/content",
      "/api/v1/posts/10/files/0/content",
    ]);
    Object.defineProperties(images[0], {
      naturalWidth: { configurable: true, value: 1_000 },
      naturalHeight: { configurable: true, value: 500 },
    });
    Object.defineProperties(images[1], {
      naturalWidth: { configurable: true, value: 2_000 },
      naturalHeight: { configurable: true, value: 1_000 },
    });
    panes.forEach((pane) => Object.defineProperties(pane, {
      clientWidth: { configurable: true, value: 500 },
      clientHeight: { configurable: true, value: 400 },
    }));
    images.forEach((image) => image.dispatchEvent(new Event("load")));
    imageComparison.querySelector<HTMLButtonElement>("[data-compare-fit]")?.click();
    expect(imageComparison.querySelector("[data-compare-zoom]")?.textContent).toBe("25%");
    expect(images.map((image) => image.style.width)).toEqual(["250px", "500px"]);
    imageComparison.querySelector<HTMLButtonElement>("[data-compare-zoom-in]")?.click();
    expect(imageComparison.querySelector("[data-compare-zoom]")?.textContent).toBe("50%");
    expect(images.map((image) => image.style.width)).toEqual(["500px", "1000px"]);
    panes[0].scrollLeft = 100;
    expect(panes[1].scrollLeft).toBe(0);

    const retainedImages = [...images];
    const oldSelect = app.shadowRoot?.querySelector<HTMLSelectElement>("[data-old-artifact]")!;
    oldSelect.value = "";
    oldSelect.dispatchEvent(new Event("change"));
    expect(retainedImages.every((image) => !image.hasAttribute("src"))).toBe(true);
  });

  test("aligns text revisions by line and renders hostile diff text inertly", async () => {
    const newer = post(10, { predecessor_post_id: 9, files: [file(0, "text", "notes.txt")] });
    const older = post(9, { files: [file(0, "text", "notes.txt")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url === "/api/v1/posts/9/files/0/content") return new Response("alpha\nold <img src=x onerror=alert(1)>\nomega");
      if (url === "/api/v1/posts/10/files/0/content") return new Response("alpha\nnew\nomega");
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();

    await vi.waitFor(() => expect(app.shadowRoot?.querySelectorAll("[data-diff-row]")).toHaveLength(4));
    const diff = app.shadowRoot?.querySelector<HTMLElement>("[data-text-comparison]")!;
    expect(Array.from(diff.querySelectorAll<HTMLElement>("[data-diff-row]")).map((row) => row.dataset.diffKind))
      .toEqual(["equal", "removed", "added", "equal"]);
    expect(diff.textContent).toContain("old <img src=x onerror=alert(1)>");
    expect(diff.querySelector("img")).toBeNull();
    expect(diff.innerHTML).not.toContain("<img src=x");
    expect(diff.textContent).toContain("Previous · text");
    expect(diff.textContent).toContain("Current · text");
    expect(Array.from(diff.querySelectorAll<HTMLAnchorElement>("a[download]")).map((link) => link.getAttribute("href"))).toEqual([
      "/api/v1/posts/9/files/0/content",
      "/api/v1/posts/10/files/0/content",
    ]);
  });

  test("pretty-prints JSON before mixed-format comparison and discloses malformed raw fallback", async () => {
    const newer = post(10, { predecessor_post_id: 9, files: [file(0, "text", "renamed.txt")] });
    const older = post(9, { files: [file(0, "json", "data.json")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url === "/api/v1/posts/9/files/0/content") return new Response('{"b":2,"a":1}');
      if (url === "/api/v1/posts/10/files/0/content") return new Response("plain text");
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();
    await rendered(app, "Comparing post 10 with post 9");
    const oldSelect = app.shadowRoot?.querySelector<HTMLSelectElement>("[data-old-artifact]")!;
    const newSelect = app.shadowRoot?.querySelector<HTMLSelectElement>("[data-new-artifact]")!;
    oldSelect.value = "0";
    oldSelect.dispatchEvent(new Event("change"));
    newSelect.value = "0";
    newSelect.dispatchEvent(new Event("change"));

    await vi.waitFor(() => expect(app.shadowRoot?.querySelector("[data-text-comparison]")?.textContent).toContain('"b": 2'));
    const comparison = app.shadowRoot?.querySelector<HTMLElement>("[data-text-comparison]")!;
    expect(comparison.textContent).toContain("Previous · json");
    expect(comparison.textContent).toContain("Current · text");
    expect(comparison.textContent?.indexOf('"b": 2')).toBeLessThan(comparison.textContent!.indexOf('"a": 1'));

    comparison.remove();
    const malformedNewer = post(12, { predecessor_post_id: 9, files: [file(0, "json", "data.json")] });
    const malformed = document.createElement("glim-text-comparison") as HTMLElement & { data: unknown };
    malformed.data = {
      older: { postId: older.id, file: older.files[0] },
      newer: { postId: malformedNewer.id, file: malformedNewer.files[0] },
    };
    vi.mocked(fetch).mockImplementation(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts/9/files/0/content") return new Response('{"ok":true}');
      if (url === "/api/v1/posts/12/files/0/content") return new Response('{"unsafe":"<script>", broken');
      throw new Error(`unexpected fetch ${url}`);
    });
    app.shadowRoot?.querySelector("[data-selected-pair]")?.append(malformed);
    await vi.waitFor(() => expect(malformed.querySelector("[data-json-fallback]")).not.toBeNull());
    expect(malformed.textContent).toContain("comparing disclosed raw text instead");
    expect(malformed.textContent).toContain('<script>');
    expect(malformed.querySelector("script")).toBeNull();
  });

  test("preserves JSON key order, duplicate keys, numeric literals, and escaped punctuation", async () => {
    const newer = post(10, { predecessor_post_id: 9, files: [file(0, "json", "data.json")] });
    const older = post(9, { files: [file(0, "json", "data.json")] });
    const oldSource = '{"2":"b","1":"a","duplicate":1,"duplicate":2,"large":900719925474099312345,"escaped":"comma, colon: braces {} [] quote \\" slash \\\\"}';
    const newSource = '{"1":"a","2":"b","duplicate":2,"large":900719925474099312346,"escaped":"comma, colon: braces {} [] quote \\" slash \\\\"}';
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url === "/api/v1/posts/9/files/0/content") return new Response(oldSource);
      if (url === "/api/v1/posts/10/files/0/content") return new Response(newSource);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();

    await vi.waitFor(() => expect(app.shadowRoot?.querySelectorAll("[data-diff-row]").length).toBeGreaterThan(0));
    const rows = Array.from(app.shadowRoot?.querySelectorAll<HTMLElement>("[data-diff-row]") ?? []);
    const previous = rows.map((row) => row.children[0]?.textContent ?? "").join("\n");
    const current = rows.map((row) => row.children[1]?.textContent ?? "").join("\n");
    expect(previous.indexOf('"2": "b"')).toBeLessThan(previous.indexOf('"1": "a"'));
    expect(previous.match(/"duplicate"/g)).toHaveLength(2);
    expect(previous).toContain("900719925474099312345");
    expect(current).toContain("900719925474099312346");
    expect(previous).toContain('"escaped": "comma, colon: braces {} [] quote \\" slash \\\\"');
    expect(rows.some((row) => row.dataset.diffKind !== "equal")).toBe(true);
  });

  test.each([
    ["depth", "depth", "[".repeat(65) + "0" + "]".repeat(65), "64-level JSON formatting depth limit"],
    ["ASCII size", "size", "[".repeat(64) + Array.from({ length: 140_000 }, () => "0").join(",") + "]".repeat(64), "16.0 MiB JSON formatting output limit"],
    ["UTF-8 size", "size", "[".repeat(64) + Array.from({ length: 70_000 }, () => `"${"é".repeat(64)}"`).join(",") + "]".repeat(64), "16.0 MiB JSON formatting output limit"],
  ])("falls back to raw text when JSON formatting exceeds the %s budget", async (_case, fallback, oversizedJson, disclosure) => {
    const newer = post(10, { predecessor_post_id: 9, files: [file(0, "json", "data.json")] });
    const older = post(9, { files: [file(0, "json", "data.json")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/content")) return new Response(oversizedJson);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();

    await vi.waitFor(() => expect(app.shadowRoot?.querySelector(`[data-json-format-fallback="${fallback}"]`)).not.toBeNull());
    const comparison = app.shadowRoot?.querySelector<HTMLElement>("[data-text-comparison]")!;
    expect(comparison.textContent).toContain(disclosure);
    expect(comparison.textContent).not.toContain("Malformed JSON");
    expect(comparison.textContent).toContain(oversizedJson.slice(0, 80));
  });

  test.each([
    ["work", Array.from({ length: 501 }, (_, index) => `old-${index}`).join("\n"), Array.from({ length: 500 }, (_, index) => `new-${index}`).join("\n"), "250,000 comparison-cell"],
    ["rows", Array.from({ length: 4_001 }, (_, index) => `same-${index}`).join("\n"), Array.from({ length: 4_001 }, (_, index) => `same-${index}`).join("\n"), "4,000 rendered-row"],
  ])("falls back to finite plain viewing when the %s diff budget is exceeded", async (budget, oldText, newText, disclosure) => {
    const newer = post(10, { predecessor_post_id: 9, files: [file(0, "text", "large.txt")] });
    const older = post(9, { files: [file(0, "text", "large.txt")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url === "/api/v1/posts/9/files/0/content") return new Response(oldText);
      if (url === "/api/v1/posts/10/files/0/content") return new Response(newText);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();

    await vi.waitFor(() => expect(app.shadowRoot?.querySelector(`[data-diff-fallback="${budget}"]`)).not.toBeNull());
    const comparison = app.shadowRoot?.querySelector<HTMLElement>("[data-text-comparison]")!;
    expect(comparison.textContent).toContain(disclosure);
    expect(comparison.querySelectorAll("pre.plain-comparison")).toHaveLength(2);
    expect(comparison.querySelector("[data-diff-row]")).toBeNull();
  });

  test("requires per-file large-document opt-in, defers the diff, and aborts both loads when deselected", async () => {
    vi.stubGlobal("IntersectionObserver", TestIntersectionObserver as unknown as typeof IntersectionObserver);
    const large = { ...file(0, "text", "notes.txt"), blob: { hash: "hidden", byte_size: 16 * 1024 * 1024 + 1 } };
    const newer = post(10, { predecessor_post_id: 9, files: [large] });
    const older = post(9, { files: [file(0, "text", "notes.txt")] });
    const signals: AbortSignal[] = [];
    const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === "/api/v1/posts") return Promise.resolve(jsonResponse({ posts: [newer], next_cursor: null }));
      if (url === "/api/v1/posts/9") return Promise.resolve(jsonResponse(older));
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      if (url.endsWith("/content")) {
        signals.push(init?.signal as AbortSignal);
        return new Promise<Response>(() => undefined);
      }
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const app = mount();
    await rendered(app, "Post 10");
    await rendered(app, "agent-session");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();

    await rendered(app, "Load full documents");
    expect(composedText(app)).toContain("16.0 MiB");
    expect(app.shadowRoot?.querySelector("[data-text-comparison]")?.querySelectorAll("a[download]")).toHaveLength(2);
    expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith("/content"))).toBe(false);
    app.shadowRoot?.querySelector<HTMLButtonElement>("[data-load-full-comparison]")?.click();
    await vi.waitFor(() => expect(TestIntersectionObserver.instances).toHaveLength(1));
    expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith("/content"))).toBe(false);
    const observer = TestIntersectionObserver.instances[0];
    const [target] = observer.observed;
    observer.trigger(target, true);
    await vi.waitFor(() => expect(signals).toHaveLength(2));

    const oldSelect = app.shadowRoot?.querySelector<HTMLSelectElement>("[data-old-artifact]")!;
    oldSelect.value = "";
    oldSelect.dispatchEvent(new Event("change"));
    expect(signals.every((signal) => signal.aborted)).toBe(true);
    app.remove();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

  test("keeps comparison source downloads available when document loading fails", async () => {
    vi.stubGlobal("IntersectionObserver", undefined);
    vi.stubGlobal("fetch", vi.fn(async () => new Response("unavailable", { status: 503 })));
    const comparison = document.createElement("glim-text-comparison") as HTMLElement & { data: unknown };
    comparison.data = {
      older: { postId: 100, file: file(0, "text", "notes.txt") },
      newer: { postId: 101, file: file(0, "text", "notes.txt") },
    };
    document.body.append(comparison);
    try {
      await vi.waitFor(() => expect(comparison.textContent).toContain("Could not load documents"));
      expect(comparison.querySelectorAll("a[download]")).toHaveLength(2);
    } finally { comparison.remove(); }
  });

  test("bounds temporary line arrays before falling back for newline-heavy documents", async () => {
    vi.stubGlobal("IntersectionObserver", undefined);
    const source = "line\n".repeat(20_000);
    vi.stubGlobal("fetch", vi.fn(async () => new Response(source)));
    const split = vi.spyOn(String.prototype, "split");
    const comparison = document.createElement("glim-text-comparison") as HTMLElement & { data: unknown };
    comparison.data = {
      older: { postId: 100, file: file(0, "text", "lines.txt") },
      newer: { postId: 101, file: file(0, "text", "lines.txt") },
    };
    document.body.append(comparison);
    try {
      await vi.waitFor(() => expect(comparison.querySelector('[data-diff-fallback="rows"]')).not.toBeNull());
      const arrays = split.mock.results.filter((result, index) => String(split.mock.contexts[index]) === source && result.type === "return");
      expect(arrays.length).toBeGreaterThan(0);
      expect(arrays.every((result) => result.value.length <= 4_001)).toBe(true);
      expect(Array.from(comparison.querySelectorAll("pre")).every((pre) => pre.textContent === source)).toBe(true);
    } finally {
      split.mockRestore();
      comparison.remove();
    }
  });

  test("shares the global three-document load budget across text comparison elements", async () => {
    vi.stubGlobal("IntersectionObserver", undefined);
    const resolvers: Array<(response: Response) => void> = [];
    const fetchMock = vi.fn((_input: RequestInfo | URL) => new Promise<Response>((resolve) => resolvers.push(resolve)));
    vi.stubGlobal("fetch", fetchMock);
    const makeComparison = (base: number) => {
      const comparison = document.createElement("glim-text-comparison") as HTMLElement & { data: unknown };
      comparison.data = {
        older: { postId: base, file: file(0, "text", `${base}.txt`) },
        newer: { postId: base + 1, file: file(0, "text", `${base}.txt`) },
      };
      return comparison;
    };
    const first = makeComparison(100);
    const second = makeComparison(200);
    document.body.append(first, second);

    await vi.waitFor(() => expect(resolvers).toHaveLength(3));
    expect(fetchMock).toHaveBeenCalledTimes(3);
    resolvers[0](new Response("released"));
    await vi.waitFor(() => expect(resolvers).toHaveLength(4));
    first.remove();
    second.remove();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

  test("services only the latest comparison target when predecessor responses arrive out of order", async () => {
    const newerTen = post(10, { predecessor_post_id: 9 });
    const newerTwenty = post(20, { predecessor_post_id: 19 });
    let resolveNine: ((response: Response) => void) | undefined;
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return Promise.resolve(jsonResponse({ posts: [newerTwenty, newerTen], next_cursor: null }));
      if (url === "/api/v1/posts/9") return new Promise<Response>((resolve) => { resolveNine = resolve; });
      if (url === "/api/v1/posts/19") return Promise.resolve(jsonResponse(post(19)));
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("#post-10 [data-compare]")?.click();
    await vi.waitFor(() => expect(resolveNine).toBeDefined());
    window.history.pushState({}, "", "#compare-20");
    window.dispatchEvent(new HashChangeEvent("hashchange"));

    await rendered(app, "Comparing post 20 with post 19");
    resolveNine?.(jsonResponse(post(9)));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(app.shadowRoot?.querySelector("[data-comparison]")?.textContent).toContain("Comparing post 20 with post 19");
    expect(app.shadowRoot?.textContent).not.toContain("Comparing post 10 with post 9");
  });

  test("does not duplicate an active comparison request and aborts it when the hash changes", async () => {
    const newer = post(10, { predecessor_post_id: 9 });
    let pageRequests = 0;
    let resolveReconciliation: ((response: Response) => void) | undefined;
    const targetSignals: AbortSignal[] = [];
    const targetResolvers: Array<(response: Response) => void> = [];
    const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === "/api/v1/posts") {
        pageRequests += 1;
        return pageRequests === 1
          ? Promise.resolve(jsonResponse({ posts: [newer], next_cursor: null }))
          : new Promise<Response>((resolve) => { resolveReconciliation = resolve; });
      }
      if (url === "/api/v1/posts/9") {
        targetSignals.push(init?.signal as AbortSignal);
        return new Promise<Response>((resolve) => targetResolvers.push(resolve));
      }
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const app = mount();
    await rendered(app, "Post 10");
    FakeEventSource.instances[0].emit("reset", {});
    await vi.waitFor(() => expect(resolveReconciliation).toBeDefined());
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();
    await vi.waitFor(() => expect(targetResolvers).toHaveLength(1));

    resolveReconciliation?.(jsonResponse({ posts: [newer], next_cursor: null }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(targetResolvers).toHaveLength(1);

    window.location.hash = "#post-10";
    await vi.waitFor(() => expect(targetSignals[0].aborted).toBe(true));
    expect(app.shadowRoot?.querySelector("#post-10")).not.toBeNull();
  });

  test("rejects a cold comparison whose revised post is outside the project route", async () => {
    setPath("/projects/42#compare-10");
    const outside = post(10, { session_id: 8, session_public_id: "3zY8Ab", predecessor_post_id: 9 });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/projects/42/posts") return jsonResponse({ posts: [], next_cursor: null });
      if (url === "/api/v1/posts/10") return jsonResponse(outside);
      if (url === "/api/v1/sessions/3zY8Ab") return jsonResponse({
        ...session,
        id: 8,
        public_id: "3zY8Ab",
        project: { ...session.project, id: 43 },
      });
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();

    await rendered(app, "Post 10 is outside this project");
    expect(app.shadowRoot?.querySelector("[data-comparison]")).toBeNull();
  });

  test("rejects a predecessor whose validated identity leaves the revised post session", async () => {
    const newer = post(10, { predecessor_post_id: 9 });
    const wrongPredecessor = post(9, { session_id: 8, session_public_id: "3zY8Ab" });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(wrongPredecessor);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();

    await rendered(app, "The predecessor relationship is invalid");
    expect(app.shadowRoot?.querySelector("[data-comparison]")).toBeNull();
  });

  test("removes a comparison when authoritative reconciliation deletes its revised post", async () => {
    const newer = post(10, { predecessor_post_id: 9 });
    let pageRequests = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") {
        pageRequests += 1;
        return pageRequests === 1
          ? jsonResponse({ posts: [newer], next_cursor: null })
          : jsonResponse({ posts: [post(11)], next_cursor: null });
      }
      if (url === "/api/v1/posts/9") return jsonResponse(post(9));
      if (url === "/api/v1/posts/10") return jsonResponse({}, 404);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();
    await rendered(app, "Comparing post 10 with post 9");

    FakeEventSource.instances[0].emit("reset", {});

    await rendered(app, "Post 10 could not be loaded (HTTP 404)");
    expect(app.shadowRoot?.querySelector("[data-comparison]")).toBeNull();
    expect(app.shadowRoot?.querySelector("#post-10")).toBeNull();
  });

  test("reuses isolated ordinary HTML artifact renderers in comparison", async () => {
    const newer = post(10, { predecessor_post_id: 9, files: [file(0, "html", "page.html")] });
    const older = post(9, { files: [file(0, "html", "page.html")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [newer], next_cursor: null });
      if (url === "/api/v1/posts/9") return jsonResponse(older);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/content")) return new Response("<script>parent.document.body.textContent='unsafe'</script>");
      if (url.endsWith("/html-capability")) {
        const match = url.match(/posts\/(\d+)\/files\/(\d+)/)!;
        return jsonResponse({ path_prefix: `/api/v1/posts/${match[1]}/files/${match[2]}/support/`, expires_in_seconds: 300 });
      }
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 10");
    app.shadowRoot?.querySelector<HTMLAnchorElement>("[data-compare]")?.click();
    await vi.waitFor(() => {
      const artifacts = Array.from(app.shadowRoot?.querySelectorAll<HTMLElement>("[data-comparison] glim-artifact") ?? []);
      expect(artifacts).toHaveLength(2);
      expect(artifacts.map((artifact) => artifact.shadowRoot?.querySelector("iframe")?.getAttribute("sandbox"))).toEqual(["", ""]);
    });
  });

  test.each(["#compare-9007199254740992", "#compare-0", "#compare-invalid"])('ignores malformed comparison target "%s" without suppressing the feed', async (hash) => {
    setPath(`/feed${hash}`);
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [post(1)], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const app = mount();

    await rendered(app, "Post 1");
    expect(app.shadowRoot?.querySelector(".feed")).not.toBeNull();
    expect(fetchMock.mock.calls.some(([input]) => String(input).includes("9007199254740992"))).toBe(false);
  });

  test("shows loading, empty, malformed, HTTP error, and retry states without injecting errors", async () => {
    let resolveFirst: ((response: Response) => void) | undefined;
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (fetchMock.mock.calls.length === 1) {
        return new Promise<Response>((resolve) => {
          resolveFirst = resolve;
        });
      }
      if (url === "/api/v1/posts") return Promise.resolve(jsonResponse({ posts: [], next_cursor: null }));
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    expect(element.shadowRoot?.textContent).toContain("Loading feed");
    resolveFirst?.(jsonResponse({ error: { message: "<img src=x onerror=alert(1)>" } }, 500));
    await rendered(element, "Feed request failed (HTTP 500)");
    expect(element.shadowRoot?.innerHTML).not.toContain("onerror");
    element.shadowRoot?.querySelector<HTMLButtonElement>("[data-retry]")?.click();
    await rendered(element, "No posts in this feed");

    fetchMock.mockImplementationOnce(async () => jsonResponse({ posts: "wrong", next_cursor: null }));
    element.shadowRoot?.querySelector<HTMLButtonElement>("[data-retry]")?.click();
  });

  test("reports malformed top-level and nested page responses and permits retry", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ posts: "wrong", next_cursor: null }))
      .mockResolvedValueOnce(jsonResponse({ posts: [{ ...post(1), files: [null] }], next_cursor: null }))
      .mockResolvedValueOnce(jsonResponse({ posts: [], next_cursor: null }));
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    await rendered(element, "The daemon returned a malformed feed response");
    element.shadowRoot?.querySelector<HTMLButtonElement>("[data-retry]")?.click();
    await rendered(element, "The daemon returned a malformed feed response");
    element.shadowRoot?.querySelector<HTMLButtonElement>("[data-retry]")?.click();
    await rendered(element, "No posts in this feed");
  });

  test("renders image and SVG through artifact image URLs with accessible keyboard zoom", async () => {
    const imagePost = post(20, {
      files: [file(0, "image", "plot.png", "Population response"), file(1, "svg", "diagram.svg")],
    });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [imagePost], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));

    const element = mount();
    await vi.waitFor(() => {
      const count = Array.from(element.shadowRoot?.querySelectorAll<HTMLElement>("glim-artifact") ?? [])
        .reduce((total, artifact) => total + (artifact.shadowRoot?.querySelectorAll("img").length ?? 0), 0);
      expect(count).toBe(2);
    });
    const images = Array.from(element.shadowRoot!.querySelectorAll<HTMLElement>("glim-artifact"))
      .flatMap((artifact) => Array.from(artifact.shadowRoot?.querySelectorAll<HTMLImageElement>("img") ?? []));
    expect(images.map((image) => image.getAttribute("src"))).toEqual([
      "/api/v1/posts/20/files/0/content",
      "/api/v1/posts/20/files/1/content",
    ]);
    expect(images[0].alt).toContain("Population response");
    expect(images[1].alt).toContain("diagram.svg");
    expect(element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector("svg")).toBeNull();

    const imageArtifact = element.shadowRoot?.querySelector("glim-artifact") as HTMLElement;
    const preview = imageArtifact.shadowRoot?.querySelector<HTMLElement>("[data-zoom-preview]");
    expect(preview?.tagName).toBe("BUTTON");
    expect(preview?.getAttribute("aria-label")).toContain("Population response");
    expect(preview?.tabIndex).toBe(0);
    preview?.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(imageArtifact.shadowRoot?.querySelector('[role="dialog"]')).toBeNull();
    preview?.click();
    const dialog = imageArtifact.shadowRoot?.querySelector<HTMLDialogElement>('[role="dialog"]');
    expect(dialog?.tagName).toBe("DIALOG");
    expect(dialog?.open).toBe(true);
    expect(dialog?.textContent).toContain("Fit to window");
    expect(dialog?.textContent).toContain("100%");
    const zoomImage = dialog?.querySelector<HTMLImageElement>("img")!;
    Object.defineProperties(zoomImage, {
      naturalWidth: { configurable: true, value: 10_000 },
      naturalHeight: { configurable: true, value: 1_000 },
    });
    vi.spyOn(window, "innerWidth", "get").mockReturnValue(390);
    vi.spyOn(window, "innerHeight", "get").mockReturnValue(800);
    zoomImage.dispatchEvent(new Event("load"));
    expect(dialog?.querySelector("output")?.textContent).toBe("3%");
    expect(zoomImage.style.width).toBe("326px");
    expect(zoomImage.style.transform).toBe("");
    expect(imageArtifact.shadowRoot?.querySelector("style")?.textContent).toMatch(/\.zoom-controls[^}]*flex-wrap: wrap/);
    dialog?.querySelectorAll<HTMLButtonElement>("button")[2]?.click();
    dialog?.querySelectorAll<HTMLButtonElement>("button")[4]?.click();
    expect(dialog?.querySelector("output")?.textContent).toBe("125%");
    preview?.click();
    expect(imageArtifact.shadowRoot?.querySelectorAll('[role="dialog"]')).toHaveLength(1);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(imageArtifact.shadowRoot?.querySelector('[role="dialog"]')).toBeNull();
    expect(imageArtifact.shadowRoot?.activeElement).toBe(preview);
  });

  test("replaces an image load failure with a filename-oriented fallback", async () => {
    const imagePost = post(21, { files: [file(0, "image", "missing.png")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [imagePost], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector("img")).toBeTruthy());
    const artifact = element.shadowRoot?.querySelector("glim-artifact")!;
    artifact.shadowRoot?.querySelector("img")?.dispatchEvent(new Event("error"));

    expect(artifact.shadowRoot?.textContent).toContain("Could not render missing.png");
    expect(artifact.shadowRoot?.querySelector<HTMLAnchorElement>("[download]")?.download).toBe("missing.png");
  });

  test("sanitizes Markdown artifacts and rewrites only exact stored support resources", async () => {
    const markdownFile = {
      ...file(0, "markdown", "report.md"),
      support_assets: [{ relative_path: "images/a b.png", blob: { hash: "hidden", byte_size: 1 } }],
    };
    const markdownPost = post(30, { files: [markdownFile] });
    const markdown = [
      "# Report",
      "![safe](images/a%20b.png)",
      "[traversal](../secret.txt)",
      "[remote](https://example.com/x)",
      "[data](data:text/html,attack)",
      "[blob](blob:https://example.com/id)",
      "[file](file:///etc/passwd)",
      "[cross post](/api/v1/posts/99/files/0/content)",
      "<img src=x onerror=alert(1)>",
    ].join("\n");
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [markdownPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url === "/api/v1/posts/30/files/0/content") return new Response(markdown);
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await rendered(element, "Report");
    const artifact = element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot!;

    expect(artifact.querySelector<HTMLImageElement>("img")?.getAttribute("src")).toBe(
      "/api/v1/posts/30/files/0/support/images/a%20b.png",
    );
    expect(artifact.innerHTML).not.toContain("onerror");
    expect(artifact.innerHTML).not.toContain("example.com");
    expect(artifact.innerHTML).not.toContain("data:text");
    expect(artifact.innerHTML).not.toContain("file://");
    expect(artifact.innerHTML).not.toContain("../secret");
    expect(artifact.innerHTML).not.toContain("/posts/99/");
  });

  test("renders exact text and structured JSON in resizable panes with fullscreen controls", async () => {
    const panePost = post(40, {
      files: [file(0, "text", "code.txt"), file(1, "json", "data.json"), file(2, "json", "broken.json")],
    });
    const requestFullscreen = vi.fn(async () => Promise.reject(new Error("fullscreen denied")));
    Object.defineProperty(HTMLElement.prototype, "requestFullscreen", { configurable: true, value: requestFullscreen });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [panePost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/0/content")) return new Response("line one\nline two\n");
      if (url.endsWith("/1/content")) return new Response('{"nested":{"value":2}}');
      if (url.endsWith("/2/content")) return new Response("{broken");
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await rendered(element, '"nested"');
    await rendered(element, "Persisted JSON is malformed");
    const artifacts = Array.from(element.shadowRoot!.querySelectorAll<HTMLElement>("glim-artifact"));
    expect(artifacts[0].shadowRoot?.querySelector("pre")?.textContent).toBe("line one\nline two\n");
    expect(artifacts[1].shadowRoot?.querySelector("pre")?.textContent).toContain('"value": 2');
    expect((artifacts[0].shadowRoot?.querySelector(".pane") as HTMLElement | null)?.style.resize).toBe("vertical");
    (artifacts[0].shadowRoot?.querySelector("[data-fullscreen]") as HTMLButtonElement | null)?.click();
    await Promise.resolve();
    expect(requestFullscreen).toHaveBeenCalledTimes(1);
    expect((artifacts[2].shadowRoot?.querySelector("[download]") as HTMLAnchorElement | null)?.download).toBe("broken.json");
  });

  test("parses CSV edge cases and bounds materialized rows and cells", async () => {
    const rows = ['name,note,empty', '"alpha, beta","line 1\nline 2",', '"quote ""inside""",x', ...Array.from({ length: 220 }, (_, i) => `${i},value,extra,ignored`)];
    const csvPost = post(50, { files: [file(0, "csv", "table.csv")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [csvPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/content")) return new Response(rows.join("\n"));
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await rendered(element, "Showing the first 200 rows");
    const artifact = element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot!;
    expect(artifact.textContent).toContain("alpha, beta");
    expect(artifact.textContent).toContain("line 1\nline 2");
    expect(artifact.textContent).toContain('quote "inside"');
    expect(artifact.querySelectorAll("tr").length).toBeLessThanOrEqual(200);
    expect(artifact.querySelectorAll("td, th").length).toBeLessThanOrEqual(20_000);
  });

  test("renders native video and audio controls and releases offscreen resources for re-entry", async () => {
    vi.stubGlobal("IntersectionObserver", TestIntersectionObserver as unknown as typeof IntersectionObserver);
    const pause = vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    const load = vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
    const play = vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue(undefined);
    const mediaPost = post(55, {
      files: [file(0, "video", "movie.mp4", "Motion"), file(1, "audio", "sound.mp3")],
    });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [mediaPost], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));

    const element = mount();
    await vi.waitFor(() => {
      const count = Array.from(element.shadowRoot?.querySelectorAll<HTMLElement>("glim-artifact") ?? [])
        .filter((artifact) => artifact.shadowRoot?.querySelector("video, audio")).length;
      expect(count).toBe(2);
    });
    const artifacts = Array.from(element.shadowRoot!.querySelectorAll<HTMLElement>("glim-artifact"));
    const media = artifacts.map((artifact) => artifact.shadowRoot?.querySelector<HTMLMediaElement>("video, audio")!);
    expect(media.map((value) => value.tagName)).toEqual(["VIDEO", "AUDIO"]);
    expect(media.every((value) => value.controls && !value.autoplay)).toBe(true);
    expect(media.map((value) => value.getAttribute("src"))).toEqual([
      "/api/v1/posts/55/files/0/content",
      "/api/v1/posts/55/files/1/content",
    ]);
    expect(play).not.toHaveBeenCalled();
    expect(TestIntersectionObserver.instances).toHaveLength(2);
    expect(TestIntersectionObserver.instances.every((observer) => observer.options?.rootMargin === "1000px 0px")).toBe(true);

    TestIntersectionObserver.instances.forEach((observer, index) => observer.trigger(media[index], false));
    expect(pause).toHaveBeenCalledTimes(2);
    expect(media.every((value) => !value.hasAttribute("src"))).toBe(true);
    TestIntersectionObserver.instances.forEach((observer, index) => observer.trigger(media[index], true));
    expect(media.map((value) => value.getAttribute("src"))).toEqual([
      "/api/v1/posts/55/files/0/content",
      "/api/v1/posts/55/files/1/content",
    ]);
    expect(play).not.toHaveBeenCalled();
    expect(load).toHaveBeenCalled();
  });

  test("renders media safely when IntersectionObserver is unavailable and cleans up on disconnect", async () => {
    vi.stubGlobal("IntersectionObserver", undefined);
    vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
    const mediaPost = post(56, { files: [file(0, "video", "movie.mp4")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [mediaPost], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector("video")).toBeTruthy());
    const video = element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector<HTMLVideoElement>("video")!;
    expect(video.getAttribute("src")).toBe("/api/v1/posts/56/files/0/content");
    element.remove();
    expect(video.getAttribute("src")).toBeNull();
  });

  test("renders a PDF in one bounded lazy native frame with open and download fallbacks", async () => {
    vi.stubGlobal("IntersectionObserver", TestIntersectionObserver as unknown as typeof IntersectionObserver);
    const pdfPost = post(57, { files: [file(0, "pdf", "paper.pdf")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [pdfPost], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector("iframe.pdf-frame")).toBeTruthy());
    const artifact = element.shadowRoot?.querySelector<HTMLElement>("glim-artifact")!;
    const frame = artifact.shadowRoot?.querySelector<HTMLIFrameElement>("iframe.pdf-frame")!;
    const links = Array.from(artifact.shadowRoot!.querySelectorAll<HTMLAnchorElement>("a"));

    expect(frame.getAttribute("src")).toBe("/api/v1/posts/57/files/0/content");
    expect(frame.loading).toBe("lazy");
    expect(frame.title).toBe("PDF: paper.pdf");
    expect(artifact.shadowRoot?.querySelector("style")?.textContent).toMatch(/\.pdf-frame[^}]*height: 70vh/);
    expect(artifact.shadowRoot?.querySelectorAll("iframe, canvas")).toHaveLength(1);
    expect(TestIntersectionObserver.instances).toHaveLength(0);
    expect(links.map((link) => link.textContent)).toEqual(["Open", "Download"]);
    expect(links[0].getAttribute("href")).toBe("/api/v1/posts/57/files/0/content");
    expect(links[0].target).toBe("_blank");
    expect(links[0].rel).toContain("noopener");
    expect(links[1].download).toBe("paper.pdf");

    artifact.remove();
    expect(frame.getAttribute("src")).toBeNull();
    document.body.append(artifact);
    await vi.waitFor(() => expect(artifact.shadowRoot?.querySelector("iframe.pdf-frame")?.getAttribute("src")).toBe("/api/v1/posts/57/files/0/content"));
  });

  test("renders HTML in a script-free sandbox with only declared resources and inert navigation surfaces", async () => {
    const htmlFile = {
      ...file(0, "html", "page.html"),
      support_assets: [
        { relative_path: "styles/main.css" },
        { relative_path: "scripts/app.js" },
        { relative_path: "images/a b.png" },
        { relative_path: "media/movie.mp4" },
        { relative_path: "media/sound.mp3" },
        { relative_path: "media/captions.vtt" },
        { relative_path: "media/poster.jpg" },
      ],
    };
    const htmlPost = post(60, { files: [htmlFile] });
    const html = `<!doctype html><html><head>
      <base href="https://attacker.test/">
      <meta http-equiv="refresh" content="0;url=https://attacker.test/">
      <meta http-equiv="content-security-policy" content="default-src *">
      <meta name="referrer" content="unsafe-url">
      <link rel="stylesheet" href="styles/main.css">
      <link rel="icon" href="images/a%20b.png">
      <script src="scripts/app.js"></script>
    </head><body>
      <img id="safe" src="images/a%20b.png" srcset="images/a%20b.png 1x, ../secret.png 2x, https://attacker.test/x.png 3x">
      <img id="data" src="data:image/png;base64,AAAA">
      <img id="remote" src="//attacker.test/x.png"><img id="malformed" src="images/%ZZ.png">
      <video src="media/movie.mp4" poster="media/poster.jpg"><source src="media/movie.mp4"><track src="media/captions.vtt"></video>
      <audio id="declared-audio" src="media/sound.mp3"></audio><audio id="cross-post" src="/api/v1/posts/999/files/0/content"></audio>
      <form action="https://attacker.test/submit"><input src="images/a%20b.png"><button formaction="https://attacker.test/other">Send</button></form>
      <a id="fragment" href="#result">Result</a><a id="external" href="https://attacker.test/">Leave</a>
      <iframe src="images/a%20b.png"></iframe><frame src="images/a%20b.png"><object data="images/a%20b.png"></object><embed src="images/a%20b.png">
    </body></html>`;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [htmlPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url === "/api/v1/posts/60/files/0/content") return new Response(html);
      if (url === "/api/v1/posts/60/files/0/html-capability") return jsonResponse({
        path_prefix: "/cap/safe/api/v1/posts/60/files/0/support/",
        expires_in_seconds: 300,
      });
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector("iframe[srcdoc]")).toBeTruthy());
    const artifact = element.shadowRoot?.querySelector<HTMLElement>("glim-artifact")!;
    const iframe = artifact.shadowRoot?.querySelector<HTMLIFrameElement>("iframe[srcdoc]")!;
    const renderedDocument = new DOMParser().parseFromString(iframe.srcdoc, "text/html");
    const support = "/cap/safe/api/v1/posts/60/files/0/support/";
    const csp = renderedDocument.querySelector<HTMLMetaElement>('meta[http-equiv="Content-Security-Policy"]')?.content ?? "";

    expect(iframe.getAttribute("sandbox")).toBe("");
    expect(iframe.getAttribute("sandbox")).not.toContain("allow-same-origin");
    expect(iframe.referrerPolicy).toBe("no-referrer");
    expect(iframe.title).toContain("page.html");
    expect(artifact.shadowRoot?.querySelector("style")?.textContent).toMatch(/\.html-frame:fullscreen[^}]*height: 100vh/);
    expect(csp).toContain("script-src 'none'");
    expect(csp).toContain("connect-src 'none'");
    expect(csp).toContain(support);
    expect(csp).not.toContain("attacker.test");
    expect(renderedDocument.querySelectorAll("base, iframe, frame, object, embed")).toHaveLength(0);
    expect(renderedDocument.querySelectorAll('meta[http-equiv="refresh"]')).toHaveLength(0);
    expect(renderedDocument.querySelector('meta[name="referrer"]')).toBeNull();
    expect(Array.from(renderedDocument.querySelectorAll("meta[http-equiv]")).filter(
      (meta) => meta.getAttribute("http-equiv")?.toLowerCase() === "content-security-policy",
    )).toHaveLength(1);
    expect(renderedDocument.querySelector<HTMLLinkElement>('link[rel="stylesheet"]')?.getAttribute("href")).toBe(`${support}styles/main.css`);
    expect(renderedDocument.querySelector('link[rel="icon"]')).toBeNull();
    expect(renderedDocument.querySelector<HTMLScriptElement>("script")?.getAttribute("src")).toBe(`${support}scripts/app.js`);
    expect(renderedDocument.querySelector<HTMLImageElement>("#safe")?.getAttribute("src")).toBe(`${support}images/a%20b.png`);
    expect(renderedDocument.querySelector<HTMLImageElement>("#safe")?.getAttribute("srcset")).toBe(`${support}images/a%20b.png 1x`);
    expect(renderedDocument.querySelector<HTMLImageElement>("#data")?.getAttribute("src")).toBe("data:image/png;base64,AAAA");
    expect(renderedDocument.querySelector<HTMLImageElement>("#remote")?.hasAttribute("src")).toBe(false);
    expect(renderedDocument.querySelector<HTMLImageElement>("#malformed")?.hasAttribute("src")).toBe(false);
    expect(renderedDocument.querySelector<HTMLVideoElement>("video")?.getAttribute("src")).toBe(`${support}media/movie.mp4`);
    expect(renderedDocument.querySelector<HTMLVideoElement>("video")?.getAttribute("poster")).toBe(`${support}media/poster.jpg`);
    expect(renderedDocument.querySelector<HTMLSourceElement>("source")?.getAttribute("src")).toBe(`${support}media/movie.mp4`);
    expect(renderedDocument.querySelector<HTMLTrackElement>("track")?.getAttribute("src")).toBe(`${support}media/captions.vtt`);
    expect(renderedDocument.querySelector<HTMLAudioElement>("#declared-audio")?.getAttribute("src")).toBe(`${support}media/sound.mp3`);
    expect(renderedDocument.querySelector<HTMLAudioElement>("#cross-post")?.hasAttribute("src")).toBe(false);
    expect(renderedDocument.querySelector<HTMLInputElement>("input")?.getAttribute("src")).toBe(`${support}images/a%20b.png`);
    expect(renderedDocument.querySelector<HTMLFormElement>("form")?.hasAttribute("action")).toBe(false);
    expect(renderedDocument.querySelectorAll("form input:not([disabled]), form button:not([disabled])")).toHaveLength(0);
    expect(renderedDocument.querySelector("button")?.hasAttribute("formaction")).toBe(false);
    expect(renderedDocument.querySelector<HTMLAnchorElement>("#fragment")?.getAttribute("href")).toBe("#result");
    expect(renderedDocument.querySelector<HTMLAnchorElement>("#external")?.hasAttribute("href")).toBe(false);
    expect(artifact.shadowRoot?.querySelector<HTMLAnchorElement>("[download]")?.download).toBe("page.html");
  });

  test("enables only sandboxed scripts after an explicit navigation-risk warning and destroys HTML contexts", async () => {
    const htmlPost = post(61, {
      files: [{ ...file(0, "html", "interactive.html"), support_assets: [{ relative_path: "app.js" }] }],
    });
    let resolveDelayedText: ((text: string) => void) | undefined;
    let contentRequests = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [htmlPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url === "/api/v1/posts/61/files/0/content") {
        contentRequests += 1;
        if (contentRequests === 1) return new Response('<script src="app.js"></script>');
        return { ok: true, text: () => new Promise<string>((resolve) => { resolveDelayedText = resolve; }) } as Response;
      }
      if (url === "/api/v1/posts/61/files/0/html-capability") return jsonResponse({
        path_prefix: "/cap/interactive/api/v1/posts/61/files/0/support/",
        expires_in_seconds: 300,
      });
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector("iframe[srcdoc]")).toBeTruthy());
    const artifact = element.shadowRoot?.querySelector<HTMLElement>("glim-artifact")!;
    const safeFrame = artifact.shadowRoot?.querySelector<HTMLIFrameElement>("iframe[srcdoc]")!;
    const safeCsp = new DOMParser().parseFromString(safeFrame.srcdoc, "text/html")
      .querySelector<HTMLMetaElement>('meta[http-equiv="Content-Security-Policy"]')!.content;
    const warning = artifact.shadowRoot?.querySelector<HTMLElement>("[data-script-warning]")!;
    expect(warning.textContent?.toLowerCase()).toContain("navigate its own frame");
    expect(warning.textContent?.toLowerCase()).toContain("network request");
    expect(safeFrame.getAttribute("sandbox")).toBe("");

    artifact.shadowRoot?.querySelector<HTMLButtonElement>("[data-enable-scripts]")?.click();
    await vi.waitFor(() => expect(artifact.shadowRoot?.querySelector<HTMLIFrameElement>("iframe[srcdoc]")?.getAttribute("sandbox")).toBe("allow-scripts"));
    const scriptFrame = artifact.shadowRoot?.querySelector<HTMLIFrameElement>("iframe[srcdoc]")!;
    const scriptCsp = new DOMParser().parseFromString(scriptFrame.srcdoc, "text/html")
      .querySelector<HTMLMetaElement>('meta[http-equiv="Content-Security-Policy"]')!.content;
    expect(scriptFrame.getAttribute("sandbox")).toBe("allow-scripts");
    expect(scriptFrame.getAttribute("sandbox")).not.toContain("allow-same-origin");
    const exactSupportScope = "http://localhost:3000/cap/interactive/api/v1/posts/61/files/0/support/";
    expect(scriptCsp).toContain(`script-src 'unsafe-inline' ${exactSupportScope}`);
    expect(scriptCsp).not.toContain("'self'");
    expect(scriptCsp).toContain("connect-src 'none'");
    expect(scriptCsp.replace(`script-src 'unsafe-inline' ${exactSupportScope}`, "script-src 'none'"))
      .toBe(safeCsp);
    expect(contentRequests).toBe(1);

    artifact.remove();
    expect(scriptFrame.srcdoc).toBe("");
    expect(scriptFrame.getAttribute("src")).toBeNull();

    const parent = element.shadowRoot?.querySelector(".files > li")!;
    parent.append(artifact);
    await vi.waitFor(() => expect(resolveDelayedText).toBeDefined());
    expect(artifact.shadowRoot?.querySelector("iframe[srcdoc]")).toBeNull();
    artifact.remove();
    resolveDelayedText?.("<p>late</p>");
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(artifact.shadowRoot?.querySelector("iframe[srcdoc]")).toBeNull();
  });

  test("uses filename-oriented download fallback for unsupported files", async () => {
    const fallbackPost = post(62, { files: [file(0, "download", "archive.bin")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [fallbackPost], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));

    const element = mount();
    await rendered(element, "archive.bin");
    const download = element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector<HTMLAnchorElement>("[download]");
    expect(download?.download).toBe("archive.bin");
    expect(download?.href).toContain("/api/v1/posts/62/files/0/content");
  });

  test("shows renderer-local fetch failure fallback and aborts artifact fetches on disconnect", async () => {
    const textPost = post(70, { files: [file(0, "text", "large.txt")] });
    let artifactSignal: AbortSignal | undefined;
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === "/api/v1/posts") return Promise.resolve(jsonResponse({ posts: [textPost], next_cursor: null }));
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      if (url.endsWith("/content")) {
        artifactSignal = init?.signal as AbortSignal;
        return new Promise<Response>(() => undefined);
      }
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(artifactSignal).toBeDefined());
    element.remove();
    expect(artifactSignal?.aborted).toBe(true);

    vi.unstubAllGlobals();
    vi.stubGlobal("IntersectionObserver", undefined);
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [textPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/content")) return new Response("failed", { status: 503 });
      throw new Error(`unexpected fetch ${url}`);
    }));
    const failed = mount();
    await rendered(failed, "Could not render large.txt");
    const failedArtifact = failed.shadowRoot?.querySelector("glim-artifact")?.shadowRoot;
    expect(failedArtifact?.querySelector<HTMLAnchorElement>("[download]")?.download).toBe("large.txt");
    expect(failedArtifact?.querySelector("[data-render-retry]")).not.toBeNull();
  });

  test.each([
    ["malicious session ID", { session_public_id: 'bad\"]' }],
    ["extreme timestamp", { published_at: 8_640_000_000_001 }],
    ["missing Git field", { git: undefined }],
    ["malformed Git object", { git: { root: "relative", branch: "", commit: "abc123" } }],
    ["invalid session identity", { session_id: 0 }],
  ])("rejects a post with %s as a malformed feed without throwing", async (_label, invalid) => {
    const fetchMock = vi.fn(async () => jsonResponse({ posts: [post(80, invalid)], next_cursor: null }));
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    await rendered(element, "The daemon returned a malformed feed response");

    expect(element.shadowRoot?.querySelector("article")).toBeNull();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  test("preserves posts when load-more fails and retries the same cursor locally", async () => {
    let paginationAttempts = 0;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [post(3), post(2)], next_cursor: "older" });
      if (url === "/api/v1/posts?cursor=older") {
        paginationAttempts += 1;
        return paginationAttempts === 1
          ? jsonResponse({ error: { message: "unsafe" } }, 503)
          : jsonResponse({ posts: [post(1)], next_cursor: null });
      }
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    await rendered(element, "Post 3");
    element.shadowRoot?.querySelector<HTMLButtonElement>("[data-load-more]")?.click();
    await rendered(element, "Could not load older posts (HTTP 503)");

    expect(Array.from(element.shadowRoot?.querySelectorAll("article") ?? []).map((article) => article.id)).toEqual(["post-3", "post-2"]);
    element.shadowRoot?.querySelector<HTMLButtonElement>("[data-pagination-retry]")?.click();
    await rendered(element, "Post 1");
    expect(fetchMock.mock.calls.filter(([input]) => String(input) === "/api/v1/posts?cursor=older")).toHaveLength(2);
  });

  test("stops pagination when a repeated cursor produces no new posts", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [post(2)], next_cursor: "stuck" });
      if (url === "/api/v1/posts?cursor=stuck") return jsonResponse({ posts: [post(2)], next_cursor: "stuck" });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await rendered(element, "Post 2");
    element.shadowRoot?.querySelector<HTMLButtonElement>("[data-load-more]")?.click();
    await rendered(element, "Pagination stopped because the daemon made no progress");

    expect(element.shadowRoot?.querySelector("[data-load-more]")).toBeNull();
  });

  test("shares an in-flight provenance lookup across pagination", async () => {
    let resolveSession: ((response: Response) => void) | undefined;
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return Promise.resolve(jsonResponse({ posts: [post(2)], next_cursor: "older" }));
      if (url === "/api/v1/posts?cursor=older") return Promise.resolve(jsonResponse({ posts: [post(1)], next_cursor: null }));
      if (url === "/api/v1/sessions/2zY8Ab") {
        return new Promise<Response>((resolve) => { resolveSession = resolve; });
      }
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    await vi.waitFor(() => expect(resolveSession).toBeDefined());
    element.shadowRoot?.querySelector<HTMLButtonElement>("[data-load-more]")?.click();
    await rendered(element, "Post 1");

    expect(fetchMock.mock.calls.filter(([input]) => String(input) === "/api/v1/sessions/2zY8Ab")).toHaveLength(1);
    resolveSession?.(jsonResponse(session));
    await rendered(element, "agent-session");
  });

  test("replaces failed, malformed, and mismatched provenance with an unavailable state", async () => {
    const posts = [
      post(91, { session_id: 7, session_public_id: "3zY8Ab" }),
      post(92, { session_id: 8, session_public_id: "4zY8Ab" }),
      post(93, { session_id: 9, session_public_id: "5zY8Ab" }),
    ];
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts, next_cursor: null });
      if (url.endsWith("/3zY8Ab")) return jsonResponse({}, 500);
      if (url.endsWith("/4zY8Ab")) return jsonResponse({ public_id: "4zY8Ab" });
      if (url.endsWith("/5zY8Ab")) return jsonResponse({ ...session, id: 9, public_id: "6zY8Ab" });
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await vi.waitFor(() => {
      const values = Array.from(element.shadowRoot?.querySelectorAll<HTMLElement>("[data-session]") ?? []);
      expect(values.map((value) => value.textContent?.startsWith("Provenance unavailable"))).toEqual([true, true, true]);
      expect(values.filter((value) => value.querySelector("[data-provenance-retry]"))).toHaveLength(1);
    });
  });

  test.each([
    ["/feed", "/api/v1/posts", ["/feed"]],
    ["/projects/42", "/api/v1/projects/42/posts", ["/projects/42", "/feed"]],
  ])("keeps %s header navigation scoped while adding per-post provenance links", async (path, endpoint, headerHrefs) => {
    setPath(path);
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === endpoint) return jsonResponse({ posts: [post(94)], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await rendered(element, "agent-session");
    const headerLinks = Array.from(element.shadowRoot?.querySelectorAll<HTMLAnchorElement>("header nav a") ?? []);
    expect(headerLinks.map((link) => link.getAttribute("href"))).toEqual(headerHrefs);
    const postProvenance = element.shadowRoot?.querySelector<HTMLElement>("article [data-session]");
    expect(postProvenance?.querySelector('a[href="/sessions/2zY8Ab"]')).not.toBeNull();
    expect(postProvenance?.querySelector('a[href="/projects/42"]')).not.toBeNull();
  });

  test("removes image sources from previews, overlays, and Markdown when disconnected", async () => {
    const markdownFile = {
      ...file(1, "markdown", "report.md"),
      support_assets: [{ relative_path: "image.png", blob: { hash: "hidden", byte_size: 1 } }],
    };
    const mediaPost = post(95, { files: [file(0, "image", "plot.png"), markdownFile] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [mediaPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/files/1/content")) return new Response("![support](image.png)");
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(element.shadowRoot?.querySelectorAll("glim-artifact")).toHaveLength(2));
    const artifacts = Array.from(element.shadowRoot!.querySelectorAll<HTMLElement>("glim-artifact"));
    await vi.waitFor(() => expect(artifacts[1].shadowRoot?.querySelector("img[src]")).not.toBeNull());
    const preview = artifacts[0].shadowRoot?.querySelector<HTMLImageElement>("img.preview")!;
    artifacts[0].shadowRoot?.querySelector<HTMLButtonElement>("[data-zoom-preview]")?.click();
    const overlay = artifacts[0].shadowRoot?.querySelector<HTMLImageElement>('[role="dialog"] img')!;
    const markdownImage = artifacts[1].shadowRoot?.querySelector<HTMLImageElement>("img")!;

    element.remove();

    expect(preview.getAttribute("src")).toBeNull();
    expect(overlay.getAttribute("src")).toBeNull();
    expect(markdownImage.getAttribute("src")).toBeNull();
  });

  test("does not append artifact content when body decoding resolves after disconnect", async () => {
    let resolveText: ((text: string) => void) | undefined;
    const markdownPost = post(96, { files: [file(0, "markdown", "delayed.md")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [markdownPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/files/0/content")) {
        return {
          ok: true,
          text: () => new Promise<string>((resolve) => { resolveText = resolve; }),
        } as Response;
      }
      throw new Error(`unexpected fetch ${url}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(resolveText).toBeDefined());
    const artifact = element.shadowRoot?.querySelector<HTMLElement>("glim-artifact")!;
    artifact.remove();
    resolveText?.("![late](image.png)");
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(artifact.shadowRoot?.querySelector(".markdown")).toBeNull();
    expect(artifact.shadowRoot?.querySelector("img[src]")).toBeNull();
  });

  test("does not render a feed when body decoding resolves after app disconnect", async () => {
    let resolveJson: ((value: unknown) => void) | undefined;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") {
        return {
          ok: true,
          json: () => new Promise<unknown>((resolve) => { resolveJson = resolve; }),
        } as Response;
      }
      throw new Error(`unexpected fetch ${String(input)}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    await vi.waitFor(() => expect(resolveJson).toBeDefined());
    element.remove();
    resolveJson?.({ posts: [post(97)], next_cursor: null });
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(element.shadowRoot?.querySelector("article")).toBeNull();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  test("turns an expired browser session into a login state", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ error: { code: "authentication_required" } }, 401);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));
    const app = mount();

    await rendered(app, "Authentication expired");
    expect(app.shadowRoot?.querySelector<HTMLAnchorElement>('a[href="/login"]')?.textContent).toBe("Sign in");
    expect(FakeEventSource.instances[0].readyState).toBe(FakeEventSource.CLOSED);
  });

  test("submits login tokens only in a bounded JSON body and clears failures", async () => {
    setPath("/login");
    const fetchMock = vi.fn(async () => jsonResponse({
      error: { code: "invalid_credentials", message: "secret daemon detail" },
    }, 401));
    vi.stubGlobal("fetch", fetchMock);
    const app = mount();
    await rendered(app, "Access token");
    const input = app.shadowRoot?.querySelector<HTMLInputElement>('input[type="password"]')!;
    expect(input.autocomplete).toBe("current-password");
    input.value = "private-token";

    app.shadowRoot?.querySelector<HTMLFormElement>("form")?.requestSubmit();

    await rendered(app, "Could not sign in");
    expect(input.value).toBe("");
    expect(composedText(app)).not.toContain("secret daemon detail");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/auth/session", expect.objectContaining({
      method: "POST",
      body: JSON.stringify({ token: "private-token" }),
    }));
    expect(window.location.href).not.toContain("private-token");
  });

  test("orders and deduplicates live posts while preserving existing renderer nodes", async () => {
    const initial = post(2, { published_at: 100 });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [initial], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));
    vi.spyOn(window, "scrollY", "get").mockReturnValue(0);

    const app = mount();
    await rendered(app, "Post 2");
    expect(FakeEventSource.instances[0].url).toBe("/api/v1/posts/events");
    const existing = app.shadowRoot?.querySelector("#post-2");
    FakeEventSource.instances[0].emit("post", post(3, { published_at: 100 }), "3");
    FakeEventSource.instances[0].emit("post", post(3, { published_at: 100 }), "3");
    await rendered(app, "Post 3");

    expect(Array.from(app.shadowRoot?.querySelectorAll("article") ?? []).map((value) => value.id)).toEqual(["post-3", "post-2"]);
    expect(app.shadowRoot?.querySelector("#post-2")).toBe(existing);
  });

  test("queues bounded live content away from the top and merges it on activation", async () => {
    let scrollY = 500;
    vi.spyOn(window, "scrollY", "get").mockImplementation(() => scrollY);
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [post(1)], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));
    const scrollTo = vi.spyOn(window, "scrollTo").mockImplementation(() => undefined);
    const app = mount();
    await rendered(app, "Post 1");
    FakeEventSource.instances[0].emit("post", post(2), "2");
    await rendered(app, "1 new post");
    expect(app.shadowRoot?.querySelector("#post-2")).toBeNull();

    scrollY = 0;
    app.shadowRoot?.querySelector<HTMLButtonElement>("[data-new-posts]")?.click();
    await rendered(app, "Post 2");
    expect(scrollTo).toHaveBeenCalledWith(expect.objectContaining({ top: 0 }));
    expect(app.shadowRoot?.activeElement?.id).toBe("post-2");
  });

  test("heartbeats only for a visible open session stream and stops on errors", async () => {
    vi.useFakeTimers();
    setPath("/sessions/2zY8Ab");
    let visibility: DocumentVisibilityState = "visible";
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => visibility });
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/posts")) return jsonResponse({ posts: [], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/heartbeat")) return jsonResponse({ updated: true, last_activity_at: 3 });
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const app = mount();
    await vi.waitFor(() => expect(FakeEventSource.instances).toHaveLength(1));
    FakeEventSource.instances[0].open();
    await vi.waitFor(() => expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith("/heartbeat"))).toBe(true));
    const count = fetchMock.mock.calls.filter(([input]) => String(input).endsWith("/heartbeat")).length;
    visibility = "hidden";
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(120_000);
    expect(fetchMock.mock.calls.filter(([input]) => String(input).endsWith("/heartbeat"))).toHaveLength(count);
    visibility = "visible";
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.waitFor(() => expect(fetchMock.mock.calls.filter(([input]) => String(input).endsWith("/heartbeat"))).toHaveLength(count + 1));
    FakeEventSource.instances[0].error();
    await vi.advanceTimersByTimeAsync(120_000);
    expect(fetchMock.mock.calls.filter(([input]) => String(input).endsWith("/heartbeat"))).toHaveLength(count + 1);
    app.remove();
    vi.useRealTimers();
  });

  test("removes closed-session posts when global reconciliation is empty", async () => {
    let pageRequests = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") {
        pageRequests += 1;
        return jsonResponse({ posts: pageRequests === 1 ? [post(1)] : [], next_cursor: null });
      }
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 1");

    FakeEventSource.instances[0].emit("session-closed", { project_id: 42, session_public_id: "2zY8Ab" });

    await rendered(app, "No posts in this feed");
    expect(app.shadowRoot?.querySelector("article")).toBeNull();
  });

  test("keeps queue overflow in reconciliation mode when more posts arrive", async () => {
    vi.spyOn(window, "scrollY", "get").mockReturnValue(500);
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [post(1)], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));
    const app = mount();
    await rendered(app, "Post 1");
    for (let id = 2; id <= 102; id += 1) FakeEventSource.instances[0].emit("post", post(id), String(id));
    FakeEventSource.instances[0].emit("post", post(103), "103");

    await rendered(app, "reload the latest posts");
    expect(composedText(app)).not.toContain("1 new post");
    expect(app.shadowRoot?.querySelector("#post-103")).toBeNull();
  });

  test("an aborted heartbeat cannot release a newer request's in-flight guard", async () => {
    vi.useFakeTimers();
    setPath("/sessions/2zY8Ab");
    let visibility: DocumentVisibilityState = "visible";
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => visibility });
    let heartbeatRequests = 0;
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith("/posts")) return Promise.resolve(jsonResponse({ posts: [], next_cursor: null }));
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      if (url.endsWith("/heartbeat")) {
        heartbeatRequests += 1;
        return new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")), { once: true });
        });
      }
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await vi.waitFor(() => expect(FakeEventSource.instances).toHaveLength(1));
    FakeEventSource.instances[0].open();
    await vi.waitFor(() => expect(heartbeatRequests).toBe(1));

    visibility = "hidden";
    document.dispatchEvent(new Event("visibilitychange"));
    visibility = "visible";
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.waitFor(() => expect(heartbeatRequests).toBe(2));
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(30_000);

    expect(heartbeatRequests).toBe(2);
    app.remove();
    vi.useRealTimers();
  });

  test("removes a deleted project feed after a session-closed event", async () => {
    setPath("/projects/42");
    let pageRequests = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/projects/42/posts") {
        pageRequests += 1;
        return pageRequests === 1
          ? jsonResponse({ posts: [post(1)], next_cursor: null })
          : jsonResponse({ error: { code: "project_not_found" } }, 404);
      }
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 1");
    FakeEventSource.instances[0].emit("session-closed", { project_id: 42, session_public_id: "2zY8Ab" });
    await rendered(app, "No posts in this feed");
    expect(app.shadowRoot?.querySelector("article")).toBeNull();
  });

  test("requires native confirmation before closing a session and shows the closed state", async () => {
    setPath("/sessions/2zY8Ab");
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith("/posts")) return jsonResponse({ posts: [post(1)], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab" && init?.method === "DELETE") return jsonResponse({ sessions_deleted: 1 });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/heartbeat")) return jsonResponse({ updated: true, last_activity_at: 3 });
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const confirm = vi.fn().mockReturnValueOnce(false).mockReturnValueOnce(true);
    Object.defineProperty(window, "confirm", { configurable: true, value: confirm });
    const app = mount();
    await rendered(app, "Post 1");
    const close = app.shadowRoot?.querySelector<HTMLButtonElement>("[data-close-session]")!;
    close.click();
    expect(confirm).toHaveBeenCalled();
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "DELETE")).toBe(false);
    close.click();
    await rendered(app, "Session closed");
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "DELETE")).toBe(true);
    expect(FakeEventSource.instances[0].readyState).toBe(FakeEventSource.CLOSED);
    expect(app.shadowRoot?.querySelector("article")).toBeNull();
  });

  test("reconnects app and artifact elements without duplicating durable content", async () => {
    const textPost = post(98, { files: [file(0, "text", "repeat.txt")] });
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [textPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/files/0/content")) return new Response("one copy");
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const element = mount();
    await rendered(element, "one copy");
    const artifact = element.shadowRoot?.querySelector<HTMLElement>("glim-artifact")!;
    const parent = artifact.parentElement!;
    artifact.remove();
    parent.append(artifact);
    await vi.waitFor(() => {
      expect(fetchMock.mock.calls.filter(([input]) => String(input).endsWith("/files/0/content"))).toHaveLength(2);
      expect(artifact.shadowRoot?.querySelectorAll("pre")).toHaveLength(1);
    });

    element.remove();
    document.body.append(element);
    await vi.waitFor(() => expect(element.shadowRoot?.querySelectorAll("main")).toHaveLength(1));
    await rendered(element, "one copy");
    expect(element.shadowRoot?.querySelectorAll("article")).toHaveLength(1);
  });

  test("invalidates an in-flight reset and reconciles again after session closure", async () => {
    let pageRequests = 0;
    let resolveStale: ((response: Response) => void) | undefined;
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") {
        pageRequests += 1;
        if (pageRequests === 1) return Promise.resolve(jsonResponse({ posts: [post(2), post(1)], next_cursor: null }));
        if (pageRequests === 2) return new Promise<Response>((resolve) => { resolveStale = resolve; });
        return Promise.resolve(jsonResponse({ posts: [], next_cursor: null }));
      }
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 1");
    FakeEventSource.instances[0].emit("reset", {});
    await vi.waitFor(() => expect(resolveStale).toBeDefined());
    FakeEventSource.instances[0].emit("session-closed", { project_id: 42, session_public_id: "2zY8Ab" });
    resolveStale?.(jsonResponse({ posts: [post(2), post(1)], next_cursor: null }));

    await rendered(app, "No posts in this feed");
    await vi.waitFor(() => expect(pageRequests).toBe(3));
    expect(app.shadowRoot?.querySelector("article")).toBeNull();
  });

  test("blocks stale pagination during an authoritative reset and restores pagination afterward", async () => {
    let pageRequests = 0;
    let resolveReset!: (response: Response) => void;
    let resolveOlder: ((response: Response) => void) | undefined;
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") {
        pageRequests += 1;
        if (pageRequests === 1) return Promise.resolve(jsonResponse({ posts: [post(6), post(5), post(4)], next_cursor: "stale" }));
        return new Promise<Response>((resolve) => { resolveReset = resolve; });
      }
      if (url.endsWith("cursor=stale")) return new Promise<Response>((resolve) => { resolveOlder = resolve; });
      if (url.endsWith("cursor=fresh")) return Promise.resolve(jsonResponse({ posts: [post(1)], next_cursor: null }));
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const app = mount();
    await rendered(app, "Post 4");
    FakeEventSource.instances[0].emit("reset", {});
    await vi.waitFor(() => expect(resolveReset).toBeDefined());
    app.shadowRoot?.querySelector<HTMLButtonElement>("[data-load-more]")?.click();
    resolveReset(jsonResponse({ posts: [post(6), post(5), post(4)], next_cursor: "fresh" }));
    await vi.waitFor(() => expect(app.shadowRoot?.querySelector<HTMLButtonElement>("[data-load-more]")?.disabled).toBe(false));
    resolveOlder?.(jsonResponse({ posts: [post(3), post(2)], next_cursor: null }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(Array.from(app.shadowRoot?.querySelectorAll("article") ?? []).map((article) => article.id))
      .toEqual(["post-6", "post-5", "post-4"]);
    expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith("cursor=stale"))).toBe(false);
    app.shadowRoot?.querySelector<HTMLButtonElement>("[data-load-more]")?.click();
    await rendered(app, "Post 1");
  });

  test("treats an initially missing session as closed without offering retry", async () => {
    setPath("/sessions/2zY8Ab");
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse({ error: { code: "session_not_found" } }, 404)));
    const app = mount();
    await rendered(app, "Session closed");
    expect(app.shadowRoot?.querySelector("[data-retry]")).toBeNull();
    expect(FakeEventSource.instances[0].readyState).toBe(FakeEventSource.CLOSED);
  });

  test.each(["#post-1", "#invalid", ""])('clears a failed deep-link message when navigating to "%s"', async (hash) => {
    setPath("/feed#post-999");
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [post(1)], next_cursor: null });
      if (url === "/api/v1/posts/999") return jsonResponse({}, 404);
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 999 could not be loaded");
    setPath(`/feed${hash}`);
    window.dispatchEvent(new Event("hashchange"));
    await vi.waitFor(() => expect(app.shadowRoot?.querySelector("[data-target-state]")).toBeNull());
    if (hash === "#post-1") expect(app.shadowRoot?.activeElement?.id).toBe("post-1");
  });

  test("treats a missing session during reset as closed", async () => {
    setPath("/sessions/2zY8Ab");
    let pageRequests = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/sessions/2zY8Ab/posts") {
        pageRequests += 1;
        return pageRequests === 1
          ? jsonResponse({ posts: [post(1)], next_cursor: null })
          : jsonResponse({ error: { code: "session_not_found" } }, 404);
      }
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 1");
    FakeEventSource.instances[0].emit("reset", {});

    await rendered(app, "Session closed");
    expect(FakeEventSource.instances[0].readyState).toBe(FakeEventSource.CLOSED);
  });

  test("an obsolete reconciliation cannot release a newer connection's guard", async () => {
    let pageRequests = 0;
    let resolveOld: ((response: Response) => void) | undefined;
    let resolveNew: ((response: Response) => void) | undefined;
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") {
        pageRequests += 1;
        if (pageRequests === 2) return new Promise<Response>((resolve) => { resolveOld = resolve; });
        if (pageRequests === 4) return new Promise<Response>((resolve) => { resolveNew = resolve; });
        return Promise.resolve(jsonResponse({ posts: [post(1)], next_cursor: null }));
      }
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 1");
    FakeEventSource.instances[0].emit("reset", {});
    await vi.waitFor(() => expect(resolveOld).toBeDefined());
    app.remove();
    document.body.append(app);
    await vi.waitFor(() => expect(FakeEventSource.instances).toHaveLength(2));
    await rendered(app, "Post 1");
    FakeEventSource.instances[1].emit("reset", {});
    await vi.waitFor(() => expect(resolveNew).toBeDefined());
    resolveOld?.(jsonResponse({ posts: [post(1)], next_cursor: null }));
    await Promise.resolve();
    FakeEventSource.instances[1].emit("reset", {});

    expect(pageRequests).toBe(4);
    resolveNew?.(jsonResponse({ posts: [post(1)], next_cursor: null }));
    await vi.waitFor(() => expect(pageRequests).toBe(5));
  });

  test("invalidates reconciliation when a live post arrives mid-flight", async () => {
    let pageRequests = 0;
    let resolveStale: ((response: Response) => void) | undefined;
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") {
        pageRequests += 1;
        if (pageRequests === 1) return Promise.resolve(jsonResponse({ posts: [post(1)], next_cursor: null }));
        if (pageRequests === 2) return new Promise<Response>((resolve) => { resolveStale = resolve; });
        return Promise.resolve(jsonResponse({ posts: [post(2), post(1)], next_cursor: null }));
      }
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      throw new Error(`unexpected fetch ${url}`);
    }));
    vi.spyOn(window, "scrollY", "get").mockReturnValue(0);
    const app = mount();
    await rendered(app, "Post 1");
    FakeEventSource.instances[0].emit("reset", {});
    await vi.waitFor(() => expect(resolveStale).toBeDefined());
    FakeEventSource.instances[0].emit("post", post(2), "2");
    resolveStale?.(jsonResponse({ posts: [post(1)], next_cursor: null }));

    await vi.waitFor(() => expect(pageRequests).toBe(3));
    await rendered(app, "Post 2");
    expect(app.shadowRoot?.querySelectorAll("#post-2")).toHaveLength(1);
  });

  test("reconciles deletions across previously loaded older pages", async () => {
    let firstPageRequests = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") {
        firstPageRequests += 1;
        return jsonResponse({ posts: [post(4), post(3)], next_cursor: firstPageRequests === 1 ? "older" : "refreshed" });
      }
      if (url === "/api/v1/posts?cursor=older") return jsonResponse({ posts: [post(2), post(1)], next_cursor: null });
      if (url === "/api/v1/posts?cursor=refreshed") return jsonResponse({ posts: [post(2)], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 4");
    app.shadowRoot?.querySelector<HTMLButtonElement>("[data-load-more]")?.click();
    await rendered(app, "Post 1");
    FakeEventSource.instances[0].emit("reset", {});

    await vi.waitFor(() => expect(app.shadowRoot?.querySelector("#post-1")).toBeNull());
    expect(Array.from(app.shadowRoot?.querySelectorAll("article") ?? []).map((article) => article.id))
      .toEqual(["post-4", "post-3", "post-2"]);
  });

  test("offers sign-in when reconciliation discovers expired authentication", async () => {
    let pageRequests = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") {
        pageRequests += 1;
        return pageRequests === 1
          ? jsonResponse({ posts: [post(1)], next_cursor: null })
          : jsonResponse({ error: { code: "authentication_required" } }, 401);
      }
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Post 1");
    FakeEventSource.instances[0].emit("reset", {});

    await rendered(app, "Authentication expired");
    expect(app.shadowRoot?.querySelector('a[href="/login"]')?.textContent).toBe("Sign in");
  });

  test("allows a bounded retry after transient provenance failure", async () => {
    let sessionRequests = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [post(1)], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") {
        sessionRequests += 1;
        return sessionRequests === 1 ? jsonResponse({}, 503) : jsonResponse(session);
      }
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await rendered(app, "Provenance unavailable");
    app.shadowRoot?.querySelector<HTMLButtonElement>("[data-provenance-retry]")?.click();

    await rendered(app, "agent-session");
    expect(sessionRequests).toBe(2);
  });

  test("bounds automatic provenance attempts while retaining manual retry", async () => {
    let sessionRequests = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [post(1)], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") { sessionRequests += 1; return jsonResponse({}, 503); }
      throw new Error(`unexpected fetch ${url}`);
    }));
    vi.spyOn(window, "scrollY", "get").mockReturnValue(0);
    const app = mount();
    await vi.waitFor(() => expect(sessionRequests).toBe(1));
    for (let id = 2; id <= 4; id += 1) {
      FakeEventSource.instances[0].emit("post", post(id), String(id));
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    await vi.waitFor(() => expect(sessionRequests).toBe(3));
    FakeEventSource.instances[0].emit("post", post(5), "5");
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(sessionRequests).toBe(3);
    const manual = app.shadowRoot?.querySelector<HTMLButtonElement>("[data-provenance-retry]");
    expect(manual?.textContent).toBe("Retry manually");
    manual?.click();
    await vi.waitFor(() => expect(sessionRequests).toBe(4));
  });

  test("updates a focused new-content notice in place", async () => {
    vi.spyOn(window, "scrollY", "get").mockReturnValue(500);
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [post(1)], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));
    const app = mount();
    await rendered(app, "Post 1");
    FakeEventSource.instances[0].emit("post", post(2), "2");
    const button = app.shadowRoot?.querySelector<HTMLButtonElement>("[data-new-posts]")!;
    button.focus();
    FakeEventSource.instances[0].emit("post", post(3), "3");

    await rendered(app, "2 new posts");
    expect(app.shadowRoot?.querySelector("[data-new-posts]")).toBe(button);
    expect(app.shadowRoot?.activeElement).toBe(button);
  });

  test("resolves and focuses a scoped post deep link outside the loaded page", async () => {
    setPath("/sessions/2zY8Ab#post-1");
    const scrollIntoView = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scrollIntoView });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/sessions/2zY8Ab/posts") return jsonResponse({ posts: [post(2)], next_cursor: null });
      if (url === "/api/v1/posts/1") return jsonResponse(post(1));
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();

    await vi.waitFor(() => expect(app.shadowRoot?.activeElement?.id).toBe("post-1"));
    expect(scrollIntoView).toHaveBeenCalled();
    expect(app.shadowRoot?.querySelector("#post-1")).not.toBeNull();
  });

  test("services the latest deep link and ignores stale target responses", async () => {
    setPath("/sessions/2zY8Ab#post-1");
    let resolveOne: ((response: Response) => void) | undefined;
    const scrollIntoView = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scrollIntoView });
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/sessions/2zY8Ab/posts") return Promise.resolve(jsonResponse({ posts: [post(3)], next_cursor: null }));
      if (url === "/api/v1/posts/1") return new Promise<Response>((resolve) => { resolveOne = resolve; });
      if (url === "/api/v1/posts/2") return Promise.resolve(jsonResponse(post(2)));
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();
    await vi.waitFor(() => expect(resolveOne).toBeDefined());
    window.history.pushState({}, "", "/sessions/2zY8Ab#post-2");
    window.dispatchEvent(new HashChangeEvent("hashchange"));

    await vi.waitFor(() => expect(app.shadowRoot?.activeElement?.id).toBe("post-2"));
    resolveOne?.(jsonResponse(post(1)));
    await Promise.resolve();
    expect(app.shadowRoot?.querySelector("#post-1")).toBeNull();
    const focusCount = scrollIntoView.mock.calls.length;
    expect(focusCount).toBeGreaterThan(0);
    FakeEventSource.instances[0].emit("reset", {});
    await vi.waitFor(() => expect(app.shadowRoot?.querySelector("#post-2")).not.toBeNull());
    expect(scrollIntoView).toHaveBeenCalledTimes(focusCount);
  });

  test("does not fetch an oversized text document before informed opt-in", async () => {
    const large = { ...file(0, "text", "large.txt"), blob: { hash: "hidden", byte_size: 16 * 1024 * 1024 + 1 } };
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [post(1, { files: [large] })], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/content")) return new Response("full document");
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const app = mount();

    await rendered(app, "Load full document");
    expect(composedText(app)).toContain("16.0 MiB");
    expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith("/content"))).toBe(false);
    app.shadowRoot?.querySelector<HTMLElement>("glim-artifact")?.shadowRoot
      ?.querySelector<HTMLButtonElement>("[data-load-full]")?.click();
    await rendered(app, "full document");
  });

  test("defers offscreen text rendering and bounds concurrent document downloads", async () => {
    vi.stubGlobal("IntersectionObserver", TestIntersectionObserver as unknown as typeof IntersectionObserver);
    const resolvers: Array<(response: Response) => void> = [];
    const files = Array.from({ length: 5 }, (_, index) => file(index, "json", `${index}.json`));
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return Promise.resolve(jsonResponse({ posts: [post(1, { files })], next_cursor: null }));
      if (url === "/api/v1/sessions/2zY8Ab") return Promise.resolve(jsonResponse(session));
      if (url.endsWith("/content")) return new Promise<Response>((resolve) => resolvers.push(resolve));
      throw new Error(`unexpected fetch ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    const app = mount();
    await vi.waitFor(() => expect(TestIntersectionObserver.instances).toHaveLength(5));
    expect(fetchMock.mock.calls.filter(([input]) => String(input).endsWith("/content"))).toHaveLength(0);
    TestIntersectionObserver.instances.forEach((observer) => {
      const [target] = observer.observed;
      observer.trigger(target, true);
    });
    await vi.waitFor(() => expect(resolvers).toHaveLength(3));
    const firstArtifact = app.shadowRoot?.querySelector<HTMLElement>("glim-artifact");
    expect(firstArtifact?.shadowRoot?.querySelector(".render-placeholder")?.textContent).toContain("Loading");
    resolvers[0](new Response("{}"));
    await vi.waitFor(() => expect(resolvers).toHaveLength(4));
    await vi.waitFor(() => expect(firstArtifact?.shadowRoot?.querySelector(".render-placeholder")).toBeNull());
    app.remove();
  });

  test("shows live connection state and groups secondary session actions", async () => {
    setPath("/sessions/2zY8Ab");
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input).endsWith("/posts")) return jsonResponse({ posts: [], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));
    const app = mount();
    await rendered(app, "Connecting");
    expect(app.shadowRoot?.querySelector("details[data-actions]")).not.toBeNull();
    FakeEventSource.instances[0].open();
    await rendered(app, "Live");
    FakeEventSource.instances[0].error();
    await rendered(app, "Reconnecting");
  });

  test("discloses file type and size in a consistent artifact toolbar", async () => {
    const textPost = post(1, { files: [{ ...file(0, "text", "notes.txt"), media_type: "text/plain" }] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [textPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/content")) return new Response("notes");
      throw new Error(`unexpected fetch ${url}`);
    }));
    vi.stubGlobal("navigator", { clipboard: undefined });
    const app = mount();
    await rendered(app, "notes");
    const toolbar = app.shadowRoot?.querySelector<HTMLElement>("glim-artifact")?.shadowRoot?.querySelector("[data-artifact-toolbar]");
    expect(toolbar?.textContent).toContain("text/plain");
    expect(toolbar?.textContent).toContain("12 B");
    const copy = toolbar?.querySelector<HTMLButtonElement>("[data-copy-link]");
    expect(copy?.disabled).toBe(true);
    expect(copy?.textContent).toBe("Copy unavailable");
    expect(toolbar?.querySelector('a[target="_blank"]')).not.toBeNull();
  });

  test("discloses CSV column clipping and parse errors with pane controls", async () => {
    const header = Array.from({ length: 105 }, (_, index) => `column-${index}`).join(",");
    const csvPost = post(1, { files: [file(0, "csv", "wide.csv")] });
    Object.defineProperty(HTMLElement.prototype, "requestFullscreen", { configurable: true, value: vi.fn(async () => undefined) });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [csvPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/content")) return new Response(`${header}\n${Array.from({ length: 12 }, () => '"x"oops,y').join("\n")}`);
      throw new Error(`unexpected fetch ${url}`);
    }));
    const app = mount();

    await rendered(app, "CSV parse issues");
    const artifact = app.shadowRoot?.querySelector<HTMLElement>("glim-artifact")?.shadowRoot!;
    expect(artifact.textContent).toContain("Showing the first 100 of 105 columns");
    expect(artifact.querySelector(".table-wrap")?.getAttribute("style")).toContain("resize: vertical");
    expect(artifact.querySelector("[data-fullscreen]")).not.toBeNull();
    expect(artifact.querySelectorAll(".csv-errors li")).toHaveLength(10);
    expect(artifact.textContent).toContain("Showing the first 10 of");
  });
});
