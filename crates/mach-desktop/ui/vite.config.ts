import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { viteSingleFile } from "vite-plugin-singlefile";

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  // Bundle everything into a single index.html: JS, CSS, even images. This
  // avoids Tauri's asset protocol having to serve separate files — the
  // entire frontend ships as one inlined file the WebView loads in one shot.
  plugins: [svelte(), viteSingleFile()],
  base: "./",

  // Vite options tailored for Tauri development.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: false,
    hmr: {
      protocol: "ws",
      host: "localhost",
      port: 1421,
    },
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "esnext",
    minify: "esbuild",
    sourcemap: false,
    outDir: "dist",
  },
}));
