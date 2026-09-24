import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";

const host = process.env.TAURI_DEV_HOST;

/**
 * vendor 分包：把体积大、变更少的依赖拆成独立 chunk。
 *
 * 为什么用**函数**而不是 `{ react: ["react", "react-dom"] }` 的对象形式（审计 NEW-29）：
 * 对象形式的键值是"模块 id 字面量"，要靠解析器认出来，而它对本仓的 react 家族**静默失效**——
 * 实测产物里只留下一个 173 字节的 `react-*.js` 空壳，react-dom 的实现被塞进了 `virtual-*.js`
 * （166KB）。分包失效不会有编译错误、也不会有构建警告，正是 ci.yml 那条注释点名要防的形态。
 * 函数形式拿到的是**已经解析好的模块绝对路径**，按包名判断，绕开这层解析。
 *
 * 注意**不要**把 `@codemirror/lang-*` / `legacy-modes` 列进来——那些在 CodeViewer 里是按需
 * `import()` 的（见 langLoaders），显式列进 manualChunks 会把它们拉回静态图、失去按需加载。
 * 这里只列启动即用到的核心运行时。
 */
const VENDOR_CHUNKS: Array<[chunk: string, packages: string[]]> = [
  // scheduler 是 react-dom 的运行时依赖，跟它同进同出；`react-dom/client`、`react/jsx-runtime`
  // 这些子路径都归属同名包，因此只列包名
  ["react", ["react", "react-dom", "scheduler"]],
  [
    "codemirror",
    [
      "@codemirror/view",
      "@codemirror/state",
      "@codemirror/language",
      "@codemirror/commands",
      "@codemirror/search",
      "@codemirror/theme-one-dark",
      "@lezer/common",
    ],
  ],
  ["dnd", ["@dnd-kit/core", "@dnd-kit/sortable", "@dnd-kit/utilities"]],
  ["virtual", ["@tanstack/react-virtual"]],
];

/**
 * 从模块 id 取 npm 包名（含 scope）；不在 node_modules 里的返回 null。
 *
 * 取**最后一个** `node_modules/` 之后那一段：pnpm 的布局是
 * `node_modules/.pnpm/react@18.3.1/node_modules/react/index.js`，第一个 node_modules 后面跟的是
 * `.pnpm`，只有最后一个之后才是真正的包名。
 */
function packageNameOf(id: string): string | null {
  const normalized = id.replace(/\\/g, "/");
  const marker = "node_modules/";
  const at = normalized.lastIndexOf(marker);
  if (at < 0) return null;
  const parts = normalized.slice(at + marker.length).split("/");
  return parts[0].startsWith("@") ? `${parts[0]}/${parts[1]}` : parts[0];
}

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
        manualChunks(id: string): string | undefined {
          const pkg = packageNameOf(id);
          if (!pkg) return undefined;
          return VENDOR_CHUNKS.find(([, packages]) => packages.includes(pkg))?.[0];
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
