/**
 * base64 → 字节数组（无任何 import，Node 测试可直接跑）。
 *
 * PDF / DOCX 预览都从后端拿 base64 字符串：pdf.js 要 `Uint8Array`，docx-preview
 * （JSZip）也接受字节。两处各写一份逐字符循环迟早只修一处，所以收在这里。
 */
export function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}
