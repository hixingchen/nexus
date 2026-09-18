import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  clearScreen: false,
  build: {
    rollupOptions: {
      output: {
        /**
         * vendor 分包：把体积大、变更少的依赖拆成独立 chunk。
         *
         * 注意**不要**把 `@codemirror/lang-*` / `legacy-modes` 列进来——那些在
         * CodeViewer 里是按需 `import()` 的（见 langLoaders），显式列进 manualChunks
         * 会把它们拉回静态图、失去按需加载。这里只列启动即用到的核心运行时。
         */
        manualChunks: {
          react: ["react", "react-dom", "react-dom/client"],
          codemirror: [
            "@codemirror/view",
            "@codemirror/state",
            "@codemirror/language",
            "@codemirror/commands",
            "@codemirror/search",
            "@codemirror/theme-one-dark",
            "@lezer/common",
          ],
          dnd: ["@dnd-kit/core", "@dnd-kit/sortable", "@dnd-kit/utilities"],
          virtual: ["@tanstack/react-virtual"],
        },
      },
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
}));
