import { useEffect, useState } from 'react';
import { readImageData } from '../../services/editor';

/** 扩展名 → MIME（拼 data URL 用，webview 原生解码） */
const MIME: Record<string, string> = {
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

/** 跨标签缓存 data URL，切换标签不重复读盘 */
const cache = new Map<string, string>();

/** 图片内建预览（data URL，不依赖系统默认程序） */
export function ImageViewer({ path, name }: { path: string; name: string }) {
  const [src, setSrc] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    const cached = cache.get(path);
    if (cached) {
      setSrc(cached);
      setErr(null);
      return;
    }
    let alive = true;
    setSrc(null);
    setErr(null);
    readImageData(path)
      .then(b64 => {
        const ext = path.split('.').pop()?.toLowerCase() ?? '';
        const url = `data:${MIME[ext] ?? 'application/octet-stream'};base64,${b64}`;
        cache.set(path, url);
        if (alive) setSrc(url);
      })
      .catch(e => {
        if (alive) setErr(String(e));
      });
    return () => { alive = false; };
  }, [path]);

  return (
    <div className="h-full flex items-center justify-center overflow-auto p-6 bg-nexus-editor">
      {err ? (
        <div className="text-[12px] text-nexus-error">预览失败：{err}</div>
      ) : src ? (
        <img
          src={src}
          alt={name}
          className="max-w-full max-h-full object-contain select-none"
          draggable={false}
        />
      ) : (
        <div className="text-[12px] text-nexus-muted">加载中…</div>
      )}
    </div>
  );
}
