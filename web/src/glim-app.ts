const template = document.createElement("template");
template.innerHTML = `
  <style>
    :host {
      display: block;
      font-family: system-ui, sans-serif;
      margin: 4rem auto;
      max-width: 42rem;
      padding: 0 1.5rem;
    }
  </style>
  <main>
    <h1>Glimse</h1>
    <p>Visual output from terminal-based AI agents will appear here.</p>
  </main>
`;

class GlimApp extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: "open" }).append(template.content.cloneNode(true));
  }
}

customElements.define("glim-app", GlimApp);
