import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

const pdfMocks = vi.hoisted(() => ({
  getDocument: vi.fn(),
  workerOptions: { workerSrc: "" },
}));

vi.mock("pdfjs-dist", () => ({
  getDocument: pdfMocks.getDocument,
  GlobalWorkerOptions: pdfMocks.workerOptions,
}));

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
  });

  afterEach(() => {
    document.body.replaceChildren();
    pdfMocks.getDocument.mockReset();
    pdfMocks.workerOptions.workerSrc = "";
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
    expect(imageArtifact.shadowRoot?.querySelectorAll('[role="dialog"]')).toHaveLength(1);
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

  test("materializes PDF pages lazily in sequence and bounds live canvases", async () => {
    vi.stubGlobal("IntersectionObserver", TestIntersectionObserver as unknown as typeof IntersectionObserver);
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({} as CanvasRenderingContext2D);
    const cleanup = vi.fn();
    const cancel = vi.fn();
    const getPage = vi.fn(async (pageNumber: number) => ({
      getViewport: ({ scale }: { scale: number }) => ({ width: 600 * scale, height: 800 * scale }),
      render: vi.fn(() => ({ promise: Promise.resolve(), cancel })),
      cleanup,
      pageNumber,
    }));
    const cleanupDocument = vi.fn(async () => undefined);
    const document = { numPages: 6, getPage, cleanup: cleanupDocument };
    const destroyLoading = vi.fn(async () => undefined);
    pdfMocks.getDocument.mockReturnValue({ promise: Promise.resolve(document), destroy: destroyLoading });
    const pdfPost = post(57, { files: [file(0, "pdf", "paper.pdf")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [pdfPost], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelectorAll(".pdf-page")).toHaveLength(6));
    const artifact = element.shadowRoot?.querySelector<HTMLElement>("glim-artifact")!;
    const pages = Array.from(artifact.shadowRoot!.querySelectorAll<HTMLElement>(".pdf-page"));
    expect(pages.map((page) => page.dataset.page)).toEqual(["1", "2", "3", "4", "5", "6"]);
    expect(getPage).not.toHaveBeenCalled();
    expect(pdfMocks.getDocument).toHaveBeenCalledWith(expect.objectContaining({
      url: "/api/v1/posts/57/files/0/content",
      rangeChunkSize: 65_536,
      isEvalSupported: false,
    }));
    expect(pdfMocks.workerOptions.workerSrc).toContain("pdf.worker");
    const observer = TestIntersectionObserver.instances[0];
    for (const page of pages.slice(0, 4)) {
      Object.defineProperty(page, "clientWidth", { configurable: true, value: 600 });
      observer.trigger(page, true);
      await vi.waitFor(() => expect(page.querySelector("canvas")).not.toBeNull());
    }
    expect(artifact.shadowRoot?.querySelectorAll("canvas").length).toBeLessThanOrEqual(3);
    expect(getPage.mock.calls.map(([number]) => number)).toEqual([1, 2, 3, 4]);
    observer.trigger(pages[3], false);
    expect(pages[3].querySelector("canvas")).toBeNull();
    expect(cleanup).toHaveBeenCalled();

    element.remove();
    expect(cancel).toHaveBeenCalled();
    expect(destroyLoading).toHaveBeenCalled();
    expect(cleanupDocument).toHaveBeenCalled();
  });

  test("cancels an in-flight PDF render without appending after disconnect", async () => {
    vi.stubGlobal("IntersectionObserver", TestIntersectionObserver as unknown as typeof IntersectionObserver);
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({} as CanvasRenderingContext2D);
    let resolveRender: (() => void) | undefined;
    const cancel = vi.fn();
    const page = {
      getViewport: ({ scale }: { scale: number }) => ({ width: 600 * scale, height: 800 * scale }),
      render: vi.fn(() => ({ promise: new Promise<void>((resolve) => { resolveRender = resolve; }), cancel })),
      cleanup: vi.fn(),
    };
    const document = { numPages: 1, getPage: vi.fn(async () => page), cleanup: vi.fn(async () => undefined) };
    const loading = { promise: Promise.resolve(document), destroy: vi.fn(async () => undefined) };
    pdfMocks.getDocument.mockReturnValue(loading);
    const pdfPost = post(58, { files: [file(0, "pdf", "paper.pdf")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [pdfPost], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector(".pdf-page")).toBeTruthy());
    const artifact = element.shadowRoot?.querySelector<HTMLElement>("glim-artifact")!;
    const placeholder = artifact.shadowRoot?.querySelector<HTMLElement>(".pdf-page")!;
    Object.defineProperty(placeholder, "clientWidth", { configurable: true, value: 600 });
    TestIntersectionObserver.instances[0].trigger(placeholder, true);
    await vi.waitFor(() => expect(resolveRender).toBeDefined());
    artifact.remove();
    resolveRender?.();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(artifact.shadowRoot?.querySelector("canvas")).toBeNull();
    expect(cancel).toHaveBeenCalled();
    expect(loading.destroy).toHaveBeenCalled();
    expect(document.cleanup).toHaveBeenCalled();
  });

  test("does not materialize a PDF page that leaves the lazy margin while getPage is pending", async () => {
    vi.stubGlobal("IntersectionObserver", TestIntersectionObserver as unknown as typeof IntersectionObserver);
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({} as CanvasRenderingContext2D);
    let resolvePage: ((page: unknown) => void) | undefined;
    const cleanup = vi.fn();
    const render = vi.fn(() => ({ promise: Promise.resolve(), cancel: vi.fn() }));
    const page = {
      getViewport: ({ scale }: { scale: number }) => ({ width: 600 * scale, height: 800 * scale }),
      render,
      cleanup,
    };
    const document = {
      numPages: 1,
      getPage: vi.fn(() => new Promise((resolve) => { resolvePage = resolve; })),
      cleanup: vi.fn(async () => undefined),
    };
    pdfMocks.getDocument.mockReturnValue({ promise: Promise.resolve(document), destroy: vi.fn(async () => undefined) });
    const pdfPost = post(59, { files: [file(0, "pdf", "paper.pdf")] });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [pdfPost], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));

    const element = mount();
    await vi.waitFor(() => expect(element.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector(".pdf-page")).toBeTruthy());
    const artifact = element.shadowRoot?.querySelector<HTMLElement>("glim-artifact")!;
    const placeholder = artifact.shadowRoot?.querySelector<HTMLElement>(".pdf-page")!;
    Object.defineProperty(placeholder, "clientWidth", { configurable: true, value: 600 });
    const observer = TestIntersectionObserver.instances[0];
    observer.trigger(placeholder, true);
    await vi.waitFor(() => expect(resolvePage).toBeDefined());
    observer.trigger(placeholder, false);
    resolvePage?.(page);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(render).not.toHaveBeenCalled();
    expect(placeholder.querySelector("canvas")).toBeNull();
    expect(cleanup).toHaveBeenCalled();
  });

  test("uses pending cards and filename-oriented fallback links without active embedding", async () => {
    const pendingPost = post(60, {
      files: [file(0, "html", "page.html"), file(1, "download", "archive.bin")],
    });
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === "/api/v1/posts") return jsonResponse({ posts: [pendingPost], next_cursor: null });
      if (String(input) === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      throw new Error(`unexpected fetch ${String(input)}`);
    }));

    const element = mount();
    await rendered(element, "Renderer pending");
    expect(element.shadowRoot?.querySelectorAll("iframe, embed, object")).toHaveLength(0);
    const downloads = Array.from(element.shadowRoot!.querySelectorAll<HTMLElement>("glim-artifact"))
      .flatMap((artifact) => Array.from(artifact.shadowRoot?.querySelectorAll<HTMLAnchorElement>("[download]") ?? []));
    expect(downloads.map((link) => link.download)).toEqual(["page.html", "archive.bin"]);
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
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/v1/posts") return jsonResponse({ posts: [textPost], next_cursor: null });
      if (url === "/api/v1/sessions/2zY8Ab") return jsonResponse(session);
      if (url.endsWith("/content")) return new Response("failed", { status: 503 });
      throw new Error(`unexpected fetch ${url}`);
    }));
    const failed = mount();
    await rendered(failed, "Could not render large.txt");
    expect(failed.shadowRoot?.querySelector("glim-artifact")?.shadowRoot?.querySelector<HTMLAnchorElement>("[download]")?.download).toBe("large.txt");
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
      expect(values.map((value) => value.textContent)).toEqual([
        "Provenance unavailable",
        "Provenance unavailable",
        "Provenance unavailable",
      ]);
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
});
