import { useCallback, useEffect, useRef, useState } from 'react';
import {
  getDocument,
  GlobalWorkerOptions,
  PasswordResponses,
  RenderingCancelledException,
  TextLayer,
  type PDFDocumentLoadingTask,
  type PDFDocumentProxy,
  type RenderTask,
} from 'pdfjs-dist';
import pdfWorkerUrl from 'pdfjs-dist/build/pdf.worker.min.mjs?url';
import { readFileBase64 } from '../../services/editor';
import { base64ToBytes } from '../../utils/base64';
import { toMessage } from '../../utils/message';

// worker 走同源静态资源（Vite 的 ?url 在 dev / 打包都会产出可加载的 URL），不引 CDN。
// 打包后 CSP 的 worker-src 必须允许 'self'。
GlobalWorkerOptions.workerSrc = pdfWorkerUrl;

/** 中文 PDF 的 CMap 与标准字体：由 vite.config.ts 的静态复制插件放到 dist/pdfjs/ 下。
 *  缺了它们，中文 PDF 会缺字、非内嵌标准字体会渲染失败。 */
const CMAP_URL = '/pdfjs/cmaps/';
const STANDARD_FONT_DATA_URL = '/pdfjs/standard_fonts/';

const MIN_SCALE = 0.25;
const MAX_SCALE = 4;

type PasswordPrompt = {
  /** pdf.js 给的「用这个密码重试」回调 */
  submit: (password: string) => void;
  /** 上一次密码错误 */
  incorrect: boolean;
};

function clampScale(value: number): number {
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, value));
}

/** PDF 只读预览：翻页、缩放、适应宽度、密码输入。 */
export function PdfViewer({ path, name }: { path: string; name: string }) {
  const [doc, setDoc] = useState<PDFDocumentProxy | null>(null);
  const [numPages, setNumPages] = useState(0);
  const [page, setPage] = useState(1);
  /** 'fit' = 适应宽度；数字 = 用户选定的缩放比例 */
  const [zoom, setZoom] = useState<'fit' | number>('fit');
  /** 实际渲染用的缩放（'fit' 时随容器宽度变化）；工具条百分比读它 */
  const [renderedScale, setRenderedScale] = useState(1);
  const [pageInput, setPageInput] = useState('1');
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [renderError, setRenderError] = useState<string | null>(null);
  const [passwordPrompt, setPasswordPrompt] = useState<PasswordPrompt | null>(null);
  const [passwordValue, setPasswordValue] = useState('');
  const [containerWidth, setContainerWidth] = useState(0);
  /** 重新加载（读取 / 解析失败后的重试） */
  const [reloadSeq, setReloadSeq] = useState(0);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  /** canvas 上方的透明文本层：选中/复制/查找靠它 */
  const textLayerRef = useRef<HTMLDivElement | null>(null);
  /** 当前 loadingTask 的销毁入口（密码取消 / 卸载都要用它） */
  const destroyRef = useRef<(() => void) | null>(null);

  // 容器宽度 → 「适应宽度」比例。ResizeObserver 只在尺寸变化时触发，不按帧轮询。
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const update = () => setContainerWidth(el.clientWidth);
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // 打开文档（含 StrictMode 双挂载：第一份在 cleanup 里 destroy，不会留下第二个 worker）
  useEffect(() => {
    let alive = true;
    let destroyedByUser = false;
    let loadingTask: PDFDocumentLoadingTask | null = null;

    setDoc(null);
    setNumPages(0);
    setPage(1);
    setPageInput('1');
    setZoom('fit');
    setRenderedScale(1);
    setLoading(true);
    setLoadError(null);
    setRenderError(null);
    setPasswordPrompt(null);
    setPasswordValue('');

    const destroy = () => {
      destroyedByUser = true;
      setPasswordPrompt(null);
      setLoadError('已取消输入密码');
      loadingTask?.destroy().catch(() => { /* 已经结束的 task 再 destroy 会 reject，忽略 */ });
    };
    destroyRef.current = destroy;

    (async () => {
      try {
        const base64 = await readFileBase64(path);
        if (!alive) return;
        const data = base64ToBytes(base64);
        loadingTask = getDocument({
          data,
          cMapUrl: CMAP_URL,
          cMapPacked: true,
          standardFontDataUrl: STANDARD_FONT_DATA_URL,
          // 不执行 PDF 内容里的字符串代码（PostScript 函数编译等），资源只从同源静态目录取
          isEvalSupported: false,
        });
        loadingTask.onPassword = (updatePassword: (password: string) => void, reason: number) => {
          if (!alive) return;
          setPasswordValue('');
          setPasswordPrompt({
            submit: updatePassword,
            incorrect: reason === PasswordResponses.INCORRECT_PASSWORD,
          });
        };
        const pdf = await loadingTask.promise;
        if (!alive) {
          // 销毁统一走 loadingTask：它管着 worker 与文档生命周期
          void loadingTask.destroy().catch(() => { /* 已销毁 */ });
          return;
        }
        setDoc(pdf);
        setNumPages(pdf.numPages);
        setLoading(false);
      } catch (e) {
        if (!alive || destroyedByUser) return;
        setLoading(false);
        setLoadError(toMessage(e));
      }
    })();

    return () => {
      alive = false;
      destroyRef.current = null;
      if (loadingTask) loadingTask.destroy().catch(() => { /* 同上 */ });
    };
  }, [path, reloadSeq]);

  // 渲染当前页。旧 renderTask 必须 cancel：翻页很快时，上一页晚到的绘制会覆盖新页。
  useEffect(() => {
    if (!doc) return;
    const canvas = canvasRef.current;
    if (!canvas) return;

    const textLayerDiv = textLayerRef.current;
    let cancelled = false;
    let renderTask: RenderTask | null = null;
    let textLayer: TextLayer | null = null;

    (async () => {
      try {
        const pdfPage = await doc.getPage(page);
        if (cancelled) return;
        const baseViewport = pdfPage.getViewport({ scale: 1 });
        const fitScale = containerWidth > 48
          ? (containerWidth - 32) / baseViewport.width
          : 1;
        const scale = zoom === 'fit' ? clampScale(fitScale) : clampScale(zoom);
        setRenderedScale(scale);

        const viewport = pdfPage.getViewport({ scale });
        // devicePixelRatio 让高 DPI 屏不糊；上限 2 避免 4x 缩放下 canvas 过大
        const outputScale = Math.min(window.devicePixelRatio || 1, 2);
        canvas.width = Math.max(1, Math.floor(viewport.width * outputScale));
        canvas.height = Math.max(1, Math.floor(viewport.height * outputScale));
        canvas.style.width = `${viewport.width}px`;
        canvas.style.height = `${viewport.height}px`;
        const ctx = canvas.getContext('2d');
        if (!ctx) throw new Error('无法创建 canvas 绘图上下文');

        renderTask = pdfPage.render({
          canvasContext: ctx,
          viewport,
          transform: outputScale === 1 ? undefined : [outputScale, 0, 0, outputScale, 0, 0],
        });
        await renderTask.promise;
        if (!cancelled) setRenderError(null);

        // canvas 只是像素，本身没有可选中的文字。TextLayer 在这块像素上叠一层
        // 透明的绝对定位 span（位置由 viewport 换算），选中/复制才是浏览器原生行为。
        // 它依赖容器的 --scale-factor（宽度与字号都用 calc(var(--scale-factor)*…)），
        // 必须与这次渲染的 viewport 同步；层渲染失败只影响选中，不影响看图，所以单独 catch。
        if (textLayerDiv) {
          try {
            const textContent = await pdfPage.getTextContent();
            if (cancelled) return;
            textLayerDiv.replaceChildren();
            textLayerDiv.style.setProperty('--scale-factor', String(scale));
            textLayer = new TextLayer({ textContentSource: textContent, container: textLayerDiv, viewport });
            await textLayer.render();
          } catch (e) {
            if (!cancelled && !(e instanceof RenderingCancelledException)) {
              console.warn('[PdfViewer] 文本层渲染失败（仅影响选中/复制）', e);
            }
          }
        }
      } catch (e) {
        // 翻页 / 卸载导致的 cancel 不是错误
        if (cancelled || e instanceof RenderingCancelledException) return;
        setRenderError(toMessage(e));
      }
    })();

    return () => {
      cancelled = true;
      renderTask?.cancel();
      textLayer?.cancel();
      textLayerDiv?.replaceChildren();
    };
  }, [doc, page, zoom, containerWidth]);

  useEffect(() => {
    setPageInput(String(page));
  }, [page]);

  const goToPage = useCallback((next: number) => {
    setPage(Math.min(Math.max(next, 1), Math.max(numPages, 1)));
    scrollRef.current?.scrollTo({ top: 0 });
  }, [numPages]);

  const commitPageInput = useCallback(() => {
    const parsed = Number.parseInt(pageInput, 10);
    if (Number.isFinite(parsed)) goToPage(parsed);
    else setPageInput(String(page));
  }, [pageInput, page, goToPage]);

  const stepZoom = useCallback((delta: number) => {
    const current = zoom === 'fit' ? renderedScale : zoom;
    setZoom(clampScale(Math.round((current + delta) * 100) / 100));
  }, [zoom, renderedScale]);

  const percent = Math.round(renderedScale * 100);

  return (
    <div className="h-full flex flex-col bg-nexus-editor relative">
      <div className="flex items-center gap-1 px-2 h-[38px] border-b border-nexus-border bg-nexus-surface flex-shrink-0 text-[12px] text-nexus-text-muted">
        <span className="truncate max-w-[240px] mr-auto" title={name}>{name}</span>
        <button
          className="px-1.5 py-0.5 rounded hover:bg-nexus-hover/60 disabled:opacity-30"
          disabled={!doc || page <= 1}
          title="上一页"
          onClick={() => goToPage(page - 1)}
        >‹</button>
        <input
          className="w-[42px] bg-nexus-bg border border-nexus-border rounded text-center py-0.5 text-[12px] text-nexus-text outline-none focus:border-nexus-accent disabled:opacity-30"
          value={pageInput}
          disabled={!doc}
          aria-label="页码"
          onChange={e => setPageInput(e.target.value)}
          onBlur={commitPageInput}
          onKeyDown={e => { if (e.key === 'Enter') commitPageInput(); }}
        />
        <span className="text-nexus-muted">/ {numPages || '—'}</span>
        <button
          className="px-1.5 py-0.5 rounded hover:bg-nexus-hover/60 disabled:opacity-30"
          disabled={!doc || page >= numPages}
          title="下一页"
          onClick={() => goToPage(page + 1)}
        >›</button>
        <span className="w-px h-3.5 bg-nexus-border mx-1" />
        <button
          className="px-1.5 py-0.5 rounded hover:bg-nexus-hover/60 disabled:opacity-30"
          disabled={!doc}
          title="缩小"
          onClick={() => stepZoom(-0.25)}
        >−</button>
        <span className="w-[42px] text-center tabular-nums">{percent}%</span>
        <button
          className="px-1.5 py-0.5 rounded hover:bg-nexus-hover/60 disabled:opacity-30"
          disabled={!doc}
          title="放大"
          onClick={() => stepZoom(0.25)}
        >+</button>
        <button
          className={'px-2 py-0.5 rounded hover:bg-nexus-hover/60 disabled:opacity-30 ' + (
            zoom === 'fit' ? 'text-nexus-accent bg-nexus-accent/10' : ''
          )}
          disabled={!doc}
          title="适应宽度"
          onClick={() => setZoom('fit')}
        >适应宽度</button>
      </div>

      <div ref={scrollRef} className="flex-1 overflow-auto">
        {loadError ? (
          <div className="h-full flex flex-col items-center justify-center gap-3 p-6 text-center">
            <p className="text-[12px] text-nexus-error max-w-[560px] break-words">PDF 预览失败：{loadError}</p>
            <button
              className="px-3 py-1.5 text-[12px] bg-nexus-accent/15 text-nexus-accent rounded-md hover:bg-nexus-accent/25"
              onClick={() => setReloadSeq(n => n + 1)}
            >重试</button>
          </div>
        ) : loading || !doc ? (
          <div className="h-full flex items-center justify-center text-[12px] text-nexus-muted">
            加载中…
          </div>
        ) : (
          // w-max + min-w-full：放大后画布比容器宽时，让这层按内容宽度撑开、从左侧开始排，
          // 横向滚动才能看到整页；只写 flex justify-center 的话超宽内容会向两侧等分溢出，
          // 左侧那一半永远滚不到（缩小到容器内时 min-w-full 仍保证居中）。
          <div className="min-h-full w-max min-w-full flex justify-center p-4">
            <div className="relative">
              <canvas ref={canvasRef} className="block bg-white shadow-lg" />
              <div ref={textLayerRef} className="textLayer" />
              {renderError && (
                <div className="absolute inset-0 flex items-center justify-center bg-nexus-bg/80 p-4 text-center text-[12px] text-nexus-error">
                  第 {page} 页渲染失败：{renderError}
                </div>
              )}
            </div>
          </div>
        )}
      </div>

      {passwordPrompt && (
        <div className="absolute inset-0 z-10 flex items-center justify-center bg-black/40">
          <form
            className="w-[320px] rounded-lg border border-nexus-border bg-nexus-surface p-4 space-y-3 shadow-xl"
            onSubmit={e => {
              e.preventDefault();
              const prompt = passwordPrompt;
              setPasswordPrompt(null);
              setPasswordValue('');
              prompt.submit(passwordValue);
            }}
          >
            <p className="text-[13px] text-nexus-text">该 PDF 已加密，请输入密码</p>
            {passwordPrompt.incorrect && (
              <p className="text-[12px] text-nexus-error">密码错误，请重试</p>
            )}
            <input
              autoFocus
              type="password"
              className="w-full bg-nexus-bg border border-nexus-border rounded px-2 py-1.5 text-[12px] text-nexus-text outline-none focus:border-nexus-accent"
              value={passwordValue}
              onChange={e => setPasswordValue(e.target.value)}
              aria-label="PDF 密码"
            />
            <div className="flex justify-end gap-2">
              <button
                type="button"
                className="px-3 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
                onClick={() => destroyRef.current?.()}
              >取消</button>
              <button
                type="submit"
                className="px-4 py-1.5 text-[12px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover"
              >打开</button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
