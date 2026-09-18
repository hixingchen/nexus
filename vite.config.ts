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
      /**
       * 构建警告即失败：把"0 警告"从口头约定变成硬门禁。
       *
       * 为什么值得（本轮实测的教训）：README 与审计报告的附录 B 都写着"vite build 0 警告"，
       * 而工作区实际有 2 条（`@codemirror/lang-javascript`/`lang-css` 同时被静态与动态引用，
       * 动态那半拆不出 chunk、纯属无效）。**声称的口径与实际不符而无人发现**——正是这份
       * 审计报告反复点名的失败模式。这里让它不可能再静默存在。
       *
       * 若将来某个依赖发出无法消除的警告，应在此显式列出豁免并写明理由，
       * 而不是把 `throw` 改成 `console.warn` 把门禁关掉。
       */
      onwarn(warning: { message: string }) {
        throw new Error(`构建出现警告（基线是 0，见 vite.config.ts 的说明）：${warning.message}`);
      },
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
