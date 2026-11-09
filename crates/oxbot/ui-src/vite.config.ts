// Builds the page into ../ui-dist/ as three plain files (index.html, app.js,
// app.css) with fixed names, so the oxbot program can embed them and serve
// them under a strict Content-Security-Policy: no inline scripts, nothing
// loaded from anywhere else.
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "../ui-dist",
    emptyOutDir: true,
    sourcemap: false,
    // Everything in the two files: no extra chunks or asset files, and
    // images stay small enough to inline as data: URLs.
    assetsInlineLimit: 100_000,
    cssCodeSplit: false,
    modulePreload: { polyfill: false },
    rollupOptions: {
      output: {
        entryFileNames: "app.js",
        chunkFileNames: "app-[name].js",
        assetFileNames: (info) =>
          info.names?.some((n) => n.endsWith(".css")) || info.name?.endsWith(".css")
            ? "app.css"
            : "[name][extname]",
        inlineDynamicImports: true,
      },
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
