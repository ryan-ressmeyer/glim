import { defineConfig } from "vite";

export default defineConfig({
  build: {
    rollupOptions: {
      output: {
        entryFileNames: "assets/app.js",
        assetFileNames: (asset) => asset.names.some((name) => name.includes("pdf.worker"))
          ? "assets/pdf.worker.mjs"
          : "assets/[name]-[hash][extname]",
      },
    },
  },
});
