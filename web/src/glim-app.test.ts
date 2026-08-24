import { afterEach, describe, expect, test } from "vitest";

import "./glim-app";

describe("glim-app", () => {
  afterEach(() => {
    document.body.replaceChildren();
  });

  test("identifies the product as Glimse", () => {
    const element = document.createElement("glim-app");
    document.body.append(element);

    expect(element.shadowRoot?.textContent).toContain("Glimse");
  });
});
