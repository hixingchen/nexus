import { LAYOUT_KEYS, saveLayout, useLayoutStore } from '../../stores/layoutStore';
import {
  CODE_FONT_STACK,
  EDITOR_FONT_SIZE_DEFAULT,
  EDITOR_FONT_SIZE_MAX,
  EDITOR_FONT_SIZE_MIN,
  parseEditorFontSize,
} from '../../utils/editorFont';

/**
 * 设置 > 编辑器：代码视图的字号 + 预览。
 *
 * 为什么单独一个分类而不是塞进「通用」：字号只是编辑器外观的第一项（后面还有制表符宽度、
 * 自动换行、字体族…），而「通用」装的是应用级杂项（数据目录、日志、布局重置）。
 * 分类名跟着**用户会去翻的词**走——找字号的人翻的是"编辑器"。
 *
 * 为什么要有预览：字号是"改完要开个文件才知道合不合适"的那类设置，而这个面板本身是弹窗、
 * 底下的编辑器被遮住一半。预览给出的是**同一份字体栈**（`CODE_FONT_STACK`）与同一个像素值，
 * 所以它上面看着合适，回到编辑器就是合适的。
 */

/**
 * 预览里显示的示例代码。
 *
 * 为什么挑这几行：它要同时体现**等宽对齐**（缩进层级看得出来）、中文与英文混排
 * （字号的观感差异主要体现在这里）、以及一个行号列——行号列是编辑器里最容易被字号
 * 变化搞得不协调的部分（它不跟着变的话，选大字号会立刻显得别扭）。
 */
const PREVIEW_LINES: string[] = [
  '// 中文与 ASCII 混排，看字号是否协调',
  'export function clampSize(px: number) {',
  '  return Math.min(MAX, Math.max(MIN, px));',
  '}',
];

export function EditorSection() {
  /**
   * 当前字号。
   *
   * 这里**订阅** `values` 而不是自己存一份 state：改动会经 `saveLayout` 同步回 `values`
   * （见那里的说明），于是滑块、预览、以及已经打开的编辑器三处读的是同一个值，
   * 不会出现"面板上显示 16、编辑器还是 14"的中间态。
   */
  const raw = useLayoutStore((s) => s.values?.[LAYOUT_KEYS.editorFontSize]);
  const size = parseEditorFontSize(raw);
  const isDefault = size === EDITOR_FONT_SIZE_DEFAULT;

  const apply = (next: number) => {
    const clamped = Math.min(EDITOR_FONT_SIZE_MAX, Math.max(EDITOR_FONT_SIZE_MIN, Math.round(next)));
    // 走 layoutStore 的统一写入口（防抖 + 合并 + 错误上报都在那里）
    saveLayout({ [LAYOUT_KEYS.editorFontSize]: String(clamped) });
  };

  return (
    <div className="space-y-5">
      <section>
        <div className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider mb-2">
          文本字号
        </div>

        <div className="rounded-md border border-nexus-border bg-nexus-bg/30 px-3 py-2.5">
          <div className="flex items-center gap-3">
            <StepButton label="A−" title="减小字号" onClick={() => apply(size - 1)} disabled={size <= EDITOR_FONT_SIZE_MIN} />

            <input
              type="range"
              min={EDITOR_FONT_SIZE_MIN}
              max={EDITOR_FONT_SIZE_MAX}
              step={1}
              value={size}
              onChange={(e) => apply(Number(e.target.value))}
              // 滑块的读屏名：它旁边只有一个 "16 px" 的读数，说明不了这是在调什么
              aria-label="文本字号"
              className="flex-1 min-w-0 accent-nexus-accent cursor-pointer"
            />

            <StepButton label="A+" title="增大字号" onClick={() => apply(size + 1)} disabled={size >= EDITOR_FONT_SIZE_MAX} />

            {/* 固定宽度：数字位数变化时不推挤旁边的按钮（12~20 都是两位，宽度写死才谈得上"不跳"） */}
            <span className="w-[46px] text-right text-[12px] text-nexus-text font-mono tabular-nums flex-shrink-0">
              {size} px
            </span>
          </div>

          <div className="mt-2 flex items-center gap-3 text-[11px] text-nexus-muted">
            <span>
              范围 {EDITOR_FONT_SIZE_MIN}–{EDITOR_FONT_SIZE_MAX} px，默认 {EDITOR_FONT_SIZE_DEFAULT} px
            </span>
            <button
              className="px-1.5 py-0.5 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40 disabled:hover:border-nexus-border disabled:hover:text-nexus-text"
              disabled={isDefault}
              // 已经默认值时禁用而不是隐藏：按钮突然消失会让这一行跟着抖一下
              title={isDefault ? '当前就是默认字号' : `恢复为 ${EDITOR_FONT_SIZE_DEFAULT} px`}
              onClick={() => apply(EDITOR_FONT_SIZE_DEFAULT)}
            >恢复默认</button>
          </div>
        </div>

        <p className="mt-2 text-[11px] text-nexus-muted/80 leading-relaxed">
          影响<span className="text-nexus-text">代码视图</span>与
          <span className="text-nexus-text">Markdown 预览</span>（编辑器里的预览、帮助 &gt; 使用文档），
          立即生效、已打开的标签页也跟着变（不会重置光标位置与撤销记录）。
          这个值不受「通用 &gt; 恢复默认布局」影响。
        </p>
      </section>

      <section>
        <div className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider mb-2">
          预览
        </div>

        {/* 为什么放两块预览而不是一块：这两个文本面**各自有偏移量**（代码正好是设置值，
            Markdown 正文比它小 1px、其中的代码块再小 1px——见 index.css 的说明），
            只给一块的话，用户没法确认另一块到底跟没跟。这里直接把两个面摆出来。 */}
        <div className="space-y-2">
          <PreviewBlock label="代码视图">
            {/* 视觉上刻意贴近编辑器：同一个字体栈、同一组底色（#282c33 编辑器 / #4e5a5f 行号），
                以及一个行号列——这样"看着合适"才有参考价值。
                这里的 line-height 取 1.5，而 CodeMirror 由它自己的基础主题决定行高，
                两者不保证逐像素相同：预览是**同一字号、同一字体**下的观感参考，不是像素级仿真。 */}
            <div className="rounded-md border border-nexus-border bg-[#282c33] overflow-hidden">
              <div className="flex">
                <div
                  className="flex-shrink-0 select-none text-right px-2 py-2 text-[#4e5a5f] tabular-nums"
                  style={{ fontFamily: CODE_FONT_STACK, fontSize: `${size}px`, lineHeight: 1.5 }}
                  aria-hidden
                >
                  {PREVIEW_LINES.map((_, i) => (
                    <div key={i}>{i + 1}</div>
                  ))}
                </div>
                <div className="w-px bg-nexus-border flex-shrink-0" />
                <div
                  className="flex-1 min-w-0 px-2 py-2 text-nexus-text overflow-x-auto"
                  style={{ fontFamily: CODE_FONT_STACK, fontSize: `${size}px`, lineHeight: 1.5 }}
                >
                  {PREVIEW_LINES.map((line, i) => (
                    // 空行也要占位（与编辑器一致），所以不能用 filter 把空串滤掉
                    <div key={i} className="whitespace-pre">{line || ' '}</div>
                  ))}
                </div>
              </div>
            </div>
          </PreviewBlock>

          <PreviewBlock label="Markdown 预览">
            {/* 直接套 `.md-preview`（编辑器预览与「使用文档」用的同一个类），而不是另写一套
                近似样式：那样预览里的 md 会和真实渲染分叉，而分叉出来的那一份恰恰是给人看
                的——"预览说好看、真打开却不一样"就是这么来的。 */}
            <div className="md-preview rounded-md border border-nexus-border bg-[#282c33] px-3 py-2 max-h-[132px] overflow-auto">
              <h3>标题（按比例缩放）</h3>
              <p>
                正文与<strong>强调</strong>跟着设置走，行内 <code>code</code> 按比例缩放；
                改动会同时作用到编辑器里的预览与「帮助 &gt; 使用文档」。
              </p>
              <pre><code>{'// 代码块比正文再小 1px（改动前就是 12/13 的关系）'}</code></pre>
            </div>
          </PreviewBlock>
        </div>
      </section>
    </div>
  );
}

/** 预览块的小标题（两块预览共用，免得标题写法各写一套） */
function PreviewBlock({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="text-[11px] text-nexus-muted mb-1">{label}</div>
      {children}
    </div>
  );
}

/**
 * 「A−」「A+」小按钮。
 *
 * 为什么要它（有滑块了还不够）：滑块拖到 12~20 这个范围里做 ±1 的微调很不顺手
 * （总行程只有十来像素一个刻度），而"再大一号"正是这个设置最常见的用法。
 */
function StepButton({ label, title, onClick, disabled }: {
  label: string;
  title: string;
  onClick: () => void;
  disabled: boolean;
}) {
  return (
    <button
      className="w-[34px] h-[22px] flex-shrink-0 rounded border border-nexus-border text-[12px] text-nexus-text hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-30 disabled:hover:border-nexus-border disabled:hover:text-nexus-text transition-colors"
      title={title}
      aria-label={title}
      disabled={disabled}
      onClick={onClick}
    >{label}</button>
  );
}
