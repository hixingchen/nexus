/**
 * Node ESM 解析钩子：让 `node --test` 能直接跑仓库里的 `.ts` 源码（含 stores/）。
 *
 * 为什么需要：本仓库的相对 import 不带扩展名（Vite/bundler 风格），而 Node 的 ESM
 * 解析要求显式扩展名，于是 `import '../utils/notify'` 在测试进程里找不到文件。
 * 这里在解析失败时补 `.ts` / `/index.ts` 再试一次——**只影响测试进程**，
 * 构建与运行期仍走 Vite 的解析规则（源码一个字都不用改）。
 *
 * 用 `node --import ./test/ts-hooks.mjs --test ...` 启用（见 package.json 的 test 脚本）。
 * 注意：JSX 无法被 Node 的类型擦除处理，因此测试能覆盖的是 .ts 模块链
 * （utils / stores / services），不是 .tsx 组件。
 */
import { register } from 'node:module';

register(import.meta.url);

export async function resolve(specifier, context, nextResolve) {
  try {
    return await nextResolve(specifier, context);
  } catch (err) {
    // 只补相对路径且确实没写扩展名的情形；其余（包名、带扩展名）原样抛出
    if (!specifier.startsWith('.') || /\.[cm]?[jt]sx?$/.test(specifier)) throw err;
    for (const suffix of ['.ts', '.tsx', '/index.ts']) {
      try {
        return await nextResolve(specifier + suffix, context);
      } catch {
        // 继续试下一个后缀
      }
    }
    throw err;
  }
}
