/**
 * 构建独立的 `nexus-boot-runner.exe`，并按 Tauri externalBin 的命名规则
 * 放到 `src-tauri/binaries/nexus-boot-runner-<target-triple>.exe`。
 *
 * 为什么需要这个脚本：Tauri 的 externalBin 在打包时只认带目标三元组后缀的现成文件，
 * 而 Cargo 不会自动把第二个 bin 放到那个位置。`tauri build` 的 beforeBuildCommand
 * 会先跑本脚本，再把 runner 一起打进安装包。
 *
 * 用法：
 *   node scripts/build-boot-runner.mjs          # release
 *   node scripts/build-boot-runner.mjs --debug  # debug（tauri dev 用）
 */
import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const debug = process.argv.includes('--debug');
const profile = debug ? 'debug' : 'release';

function hostTriple() {
  const out = execFileSync('rustc', ['-vV'], { encoding: 'utf8' });
  const match = out.match(/^host:\s*(\S+)$/m);
  if (!match) throw new Error('无法从 rustc -vV 解析 host target triple');
  return match[1];
}

const triple = process.env.TAURI_ENV_TARGET_TRIPLE || hostTriple();
const args = [
  'build',
  '--manifest-path', join(root, 'src-tauri', 'Cargo.toml'),
  '-p', 'nexus-boot-runner',
  '--bin', 'nexus-boot-runner',
  '--target', triple,
];
if (!debug) args.push('--release');

console.log(`[build:runner] cargo ${args.join(' ')}`);
execFileSync('cargo', args, { stdio: 'inherit', cwd: root });

const exeName = process.platform === 'win32' ? 'nexus-boot-runner.exe' : 'nexus-boot-runner';
const src = join(root, 'src-tauri', 'target', triple, profile, exeName);
if (!existsSync(src)) {
  throw new Error(`构建成功但找不到产物：${src}`);
}

const outDir = join(root, 'src-tauri', 'binaries');
mkdirSync(outDir, { recursive: true });
const suffix = process.platform === 'win32' ? '.exe' : '';
const dst = join(outDir, `nexus-boot-runner-${triple}${suffix}`);
copyFileSync(src, dst);
console.log(`[build:runner] ${src} -> ${dst}`);
