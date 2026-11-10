import { getExtension } from './path';

/**
 * 文件 → 语言键（纯函数，可被 Node 直接测）。
 *
 * 为什么需要文件名这一层：编辑器原先只按扩展名查语言包，而一批常用文件**没有可用的
 * 扩展名**，于是完全没有高亮、也不报任何错：
 * - 无点文件名此前靠 `getExtension` 的"无点返回整串"语义刚好命中（`Dockerfile` /
 *   `Makefile` / `.env`），但一带后缀就落空：`Dockerfile.dev` → dev、`Makefile.in` → in、
 *   `.env.local` → local，三个键都不在语言表里；
 * - 名字固定而扩展名通用的文件永远查不到：`CMakeLists.txt` → txt、`nginx.conf` → conf
 *   （conf 是通用配置，按 properties 高亮会把 nginx 指令当键值）；
 * - systemd 单元 `*.service` 等此前根本没登记（扩展名 service）。
 *
 * 规则：**文件名特例优先，扩展名其次**（统一小写）。返回的键由 `CodeViewer` 的
 * `langLoaders` 消费；登记新的键时要两边一起看。
 */
export function languageKeyOf(filePath: string): string {
  // 只取最后一段：Windows 的 \ 与 POSIX 的 / 都要认（树里的路径两种分隔符都会出现）
  const base = (filePath.split(/[\\/]/).pop() ?? '').toLowerCase();
  // Dockerfile 的常见变体：Dockerfile.dev / dockerfile.prod / app.dockerfile
  if (base.startsWith('dockerfile') || base.endsWith('.dockerfile')) return 'dockerfile';
  // GNU make 的 Makefile.in / Makefile.am
  if (base.startsWith('makefile')) return 'makefile';
  if (base === 'cmakelists.txt') return 'cmake';
  // nginx 的配置几乎总叫 nginx.conf，而 conf 这个扩展名是通用配置的（见上面）
  if (base === 'nginx.conf') return 'nginx';
  // dotenv 的多环境变体：.env.local / .env.production / .env.example
  if (base === '.env' || base.startsWith('.env.')) return 'env';
  return getExtension(filePath, { lower: true });
}
