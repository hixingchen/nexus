/**
 * 路径解析工具（无任何 import，可被 Node 内置测试器直接跑）。
 *
 * 为什么单独成文件：扩展名解析原先在 8 处各写一遍（两个同义局部 helper + 6 处内联），
 * 大小写处理还不一致（7 处 toLowerCase、1 处原样返回）——"扩展名一律小写"这条规则
 * 实际上靠下游 FileIcons 内部再 toLowerCase 一次兜住，解析点本身没有统一口径。
 * 规则收在一处后，图标、语言包、MIME、语言判定不会再各按各的理解走。
 */

/**
 * 取扩展名（不含点）。
 *
 * 刻意保持各处原有的 `split('.').pop() ?? ''` 语义：无点路径返回整串（不是 ''）、
 * 以点结尾返回 ''——本次只做归并，不改语义（下游既有 `=== 'jar'` 比较与 MIME 查表
 * 都按这个口径写的）。
 *
 * lower：需要大小写不敏感比较（`=== 'class'`、语言包/MIME 查表、图标名）的调用方显式传 true；
 * 需要原样大小写的调用点不传（EditorTabs 把扩展名直接交给 getIconSvg 的场景）。
 */
export function getExtension(path: string, opts?: { lower?: boolean }): string {
  const ext = path.split('.').pop() ?? '';
  return opts?.lower ? ext.toLowerCase() : ext;
}

/** 扩展名 → MIME（拼 `data:` URL 用，webview 原生解码） */
const IMAGE_MIME: Record<string, string> = {
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  svg: 'image/svg+xml',
  ico: 'image/x-icon',
  bmp: 'image/bmp',
  avif: 'image/avif',
};

/**
 * 图片扩展名 → MIME。未知扩展名给 `application/octet-stream`（与既有行为一致）。
 *
 * 为什么提到这里：图片查看器与 Markdown 预览各需要一次，而两份表**不一致时不会报错**，
 * 只表现为"某类图在某一处显示不出来"——正是这类静默漂移值得收口（本项目在扩展名解析上
 * 已经踩过一次同样的坑，见文件头注释）。
 */
export function imageMimeOf(path: string): string {
  return IMAGE_MIME[getExtension(path, { lower: true })] ?? 'application/octet-stream';
}
