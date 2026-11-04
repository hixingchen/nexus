import { useCallback, useEffect, useLayoutEffect, useRef, useState, type MouseEvent } from 'react';
import { renderAsync } from 'docx-preview';
import { readFileBase64 } from '../../services/editor';
import { base64ToBytes } from '../../utils/base64';
import { toMessage } from '../../utils/message';

const MIN_SCALE = 0.25;
const MAX_SCALE = 4;

function clampScale(value: number): number {
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, value));
}

/**
 * DOCX 只读预览。
 *
 * 用 docx-preview 把 OOXML 渲染成 DOM（不执行宏/脚本）。图片与字体由它生成 blob: URL，
 * 而它自己不会 revoke——渲染完成后从 DOM（img.src、style 文本/属性）把它们收集起来，
 * 卸载时统一回收，否则每次打开文档都会留下几十个常驻 blob。
 * CSP 的 img-src / font-src 因此要允许 blob:。
 */
export function DocxViewer({ path, name }: { path: string; name: string }) {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reloadSeq, setReloadSeq] = useState(0);
  /** 'fit' = 适应容器宽度（默认）；数字 = 用户选定的缩放比例，与 PdfViewer 同一套模型 */
  const [zoom, setZoom] = useState<'fit' | number>('fit');
  /** 实际应用的缩放（'fit' 时随容器宽度变化）；工具条百分比读它 */
  const [renderedScale, setRenderedScale] = useState(1);
  /** 每次文档渲染完成 +1：layout effect 据此给新出现的 section 套用缩放 */
  const [renderSeq, setRenderSeq] = useState(0);
  const [containerWidth, setContainerWidth] = useState(0);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  // 容器宽度 → 「适应宽度」比例（与 PdfViewer 的 ResizeObserver 同一做法）
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const update = () => setContainerWidth(el.clientWidth);
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  /**
   * 按容器宽度缩放每一页。
   *
   * 用 CSS `zoom` 而不是 `transform: scale`：zoom 会同步改变布局尺寸，
   * 滚动条高度/宽度、页面之间的间距都按缩放后的实际尺寸计算；transform 只改绘制，
   * 父级仍按原尺寸占位，滚动条长度会与实际内容对不上。
   *
   * `docx-preview` 给每页写的是固定页宽（来自 OOXML 的 pgSz，单位 pt）。渲染完成
   * 后先把原始宽度量出来存到 dataset，后续缩放/窗口变化都按它算，避免读取缩放后的宽度。
   */
  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const sections = [...container.querySelectorAll<HTMLElement>('section.docx')];
    const naturalWidth = Math.max(
      0,
      ...sections.map(s => Number(s.dataset.docxNaturalWidth) || 0),
    );
    const available = containerWidth - 48; // 外层 p-6 的左右内边距
    const fitScale = naturalWidth > 0 && available > 0 ? available / naturalWidth : 1;
    const scale = clampScale(zoom === 'fit' ? fitScale : zoom);
    for (const section of sections) {
      if (scale === 1) section.style.removeProperty('zoom');
      else section.style.setProperty('zoom', String(scale));
    }
    setRenderedScale(prev => (Math.abs(prev - scale) < 0.001 ? prev : scale));
  }, [zoom, containerWidth, renderSeq]);

  const stepZoom = useCallback((delta: number) => {
    const current = zoom === 'fit' ? renderedScale : zoom;
    setZoom(clampScale(Math.round((current + delta) * 100) / 100));
  }, [zoom, renderedScale]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let alive = true;
    /**
     * 渲染到**脱离文档的临时节点**，成功后再整体搬进可见容器。
     *
     * 为什么不直接渲染进 container：切文件时同一个 container 会被下一个 effect 复用，
     * 而 docx-preview 的异步渲染无法取消——旧文档晚到的 append/清空会覆盖新文档。
     * 临时节点让每个 effect 只碰自己的 DOM；扫 blob URL 也只扫自己这份。
     */
    const target = document.createElement('div');
    /** docx-preview 生成、且确实落进 DOM 的 blob URL（图片 src / 字体与项目符号的 CSS url()） */
    const createdUrls = new Set<string>();
    const BLOB_URL = /blob:[^\s"'()<>]+/g;
    const collectFrom = (text: string) => {
      for (const match of text.matchAll(BLOB_URL)) createdUrls.add(match[0]);
    };
    /** 从这份渲染结果里把 blob: URL 全找出来。docx-preview 没有提供句柄，只能这样回收。 */
    const collectBlobUrls = () => {
      for (const el of [target, ...target.querySelectorAll('*')]) {
        if (el.tagName === 'STYLE' && el.textContent) collectFrom(el.textContent);
        for (const attr of el.attributes) {
          if (attr.value.includes('blob:')) collectFrom(attr.value);
        }
      }
    };
    const revokeAll = () => {
      for (const url of createdUrls) URL.revokeObjectURL(url);
      createdUrls.clear();
    };

    container.innerHTML = '';
    setLoading(true);
    setError(null);
    // 换文件 / 重试时回到「适应宽度」，不把上一份文档的缩放比例带过来
    setZoom('fit');
    setRenderedScale(1);

    (async () => {
      try {
        const base64 = await readFileBase64(path);
        if (!alive) return;
        await renderAsync(base64ToBytes(base64), target, target, {
          className: 'docx',
          inWrapper: true,
          breakPages: true,
          ignoreWidth: false,
          ignoreHeight: false,
          ignoreFonts: false,
          renderHeaders: true,
          renderFooters: true,
          renderFootnotes: true,
          renderEndnotes: true,
          // blob 比 base64 省内存；CSP 已放行 blob:
          useBase64URL: false,
          experimental: false,
        });
        collectBlobUrls();
        if (!alive) {
          target.innerHTML = '';
          revokeAll();
          return;
        }
        while (target.firstChild) container.appendChild(target.firstChild);
        // 先量下未缩放时的页宽（此时还没有 zoom），后续缩放都按这个基准算
        for (const section of container.querySelectorAll<HTMLElement>('section.docx')) {
          const width = section.getBoundingClientRect().width;
          if (width > 0) section.dataset.docxNaturalWidth = String(width);
        }
        setRenderSeq(n => n + 1);
        setLoading(false);
      } catch (e) {
        // 渲染中途失败时结果里可能有半截 DOM：清掉，别让错误页下面还露着残片
        collectBlobUrls();
        target.innerHTML = '';
        revokeAll();
        if (!alive) return;
        container.innerHTML = '';
        setError(toMessage(e));
        setLoading(false);
      }
    })();

    return () => {
      alive = false;
      // 旧 effect 的结果晚到时只清理自己的临时节点；可见容器属于下一个 effect
      collectBlobUrls();
      target.innerHTML = '';
      revokeAll();
      container.innerHTML = '';
    };
  }, [path, reloadSeq]);

  /** 文档里的链接一律不导航：预览是只读的，且不能把 WebView 带去外网 */
  const blockLinks = useCallback((e: MouseEvent<HTMLDivElement>) => {
    if (e.target instanceof Element && e.target.closest('a')) e.preventDefault();
  }, []);

  return (
    <div className="h-full flex flex-col bg-nexus-editor relative">
      <div className="flex items-center gap-3 px-3 h-[38px] border-b border-nexus-border bg-nexus-surface flex-shrink-0 text-[12px] text-nexus-text-muted">
        <span className="truncate max-w-[200px] flex-shrink-0" title={name}>{name}</span>
        <span className="text-[11px] text-nexus-muted truncate min-w-0 flex-1">只读预览，版式可能与 Word 有差异</span>
        <button
          className="px-1.5 py-0.5 rounded hover:bg-nexus-hover/60 disabled:opacity-30 flex-shrink-0"
          disabled={loading || !!error}
          title="缩小"
          onClick={() => stepZoom(-0.25)}
        >−</button>
        <span className="w-[42px] text-center tabular-nums flex-shrink-0">{Math.round(renderedScale * 100)}%</span>
        <button
          className="px-1.5 py-0.5 rounded hover:bg-nexus-hover/60 disabled:opacity-30 flex-shrink-0"
          disabled={loading || !!error}
          title="放大"
          onClick={() => stepZoom(0.25)}
        >+</button>
        <button
          className={'px-2 py-0.5 rounded hover:bg-nexus-hover/60 disabled:opacity-30 flex-shrink-0 ' + (
            zoom === 'fit' ? 'text-nexus-accent bg-nexus-accent/10' : ''
          )}
          disabled={loading || !!error}
          title="适应宽度"
          onClick={() => setZoom('fit')}
        >适应宽度</button>
      </div>

      {/* docx-preview 默认 align-items:center：页面比容器宽时左侧会溢出到不可滚动的一侧。
          改成 items-start + 页面 mx-auto：放得下时居中，放不下时从左边开始、横向可滚。 */}
      <div
        ref={scrollRef}
        className="flex-1 overflow-auto bg-nexus-editor [&_.docx-wrapper]:items-start [&_.docx-wrapper]:bg-transparent [&_.docx-wrapper]:p-6 [&_.docx-wrapper>section.docx]:mx-auto [&_.docx-wrapper>section.docx]:shadow-lg"
        onClick={blockLinks}
      >
        <div ref={containerRef} />
      </div>

      {error && (
        <div className="absolute inset-x-0 bottom-0 top-[38px] flex flex-col items-center justify-center gap-3 p-6 text-center bg-nexus-editor">
          <p className="text-[12px] text-nexus-error max-w-[560px] break-words">DOCX 预览失败：{error}</p>
          <button
            className="px-3 py-1.5 text-[12px] bg-nexus-accent/15 text-nexus-accent rounded-md hover:bg-nexus-accent/25"
            onClick={() => setReloadSeq(n => n + 1)}
          >重试</button>
        </div>
      )}

      {loading && !error && (
        <div className="absolute inset-x-0 bottom-0 top-[38px] flex items-center justify-center text-[12px] text-nexus-muted bg-nexus-editor">
          渲染中…
        </div>
      )}
    </div>
  );
}
