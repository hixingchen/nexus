import { useRef, useState, useEffect, useMemo, memo } from 'react';
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, Decoration, WidgetType, type DecorationSet, type Panel } from '@codemirror/view';
import { EditorState, StateEffect, StateField, Compartment, type Range, type Text } from '@codemirror/state';
import { defaultKeymap, history, historyKeymap, redo, undo, toggleBlockCommentByLine, toggleComment } from '@codemirror/commands';
import { syntaxHighlighting, defaultHighlightStyle, indentOnInput, bracketMatching, foldGutter, foldKeymap, syntaxTree, type Language, LanguageSupport, LRLanguage, StreamLanguage, type StreamParser } from '@codemirror/language';
import { parseMixed, type SyntaxNode, type Input } from '@lezer/common';
import { search, openSearchPanel, findNext, findPrevious, closeSearchPanel, setSearchQuery, SearchQuery, highlightSelectionMatches } from '@codemirror/search';
import { createRoot } from 'react-dom/client';
import { useEditorStore, saveActiveFile, setPendingEditSource, clearPendingEditSource, settlePendingEdit } from '../../stores/editor';
import { isSubmitEnter } from '../../utils/keyboard';
import { getExtension } from '../../utils/path';
import { indentationMarkers } from '@replit/codemirror-indentation-markers';
import { oneDark } from '@codemirror/theme-one-dark';
import { javascript } from '@codemirror/lang-javascript';
import { css, cssLanguage } from '@codemirror/lang-css';
// 其余语言包改为**按需动态导入**（见 langLoaders）：静态导入会让冷启动解析全部语言包，
// 而一次会话通常只用一两种；css 例外——它被 Vue 的混合解析器（vueScssMixed）引用，
// 必须同步可用（`cssLanguage` 是解析器本体，`css` 是建 LanguageSupport 的工厂）。

interface CodeViewerProps {
  filePath: string;
  /** 是否可编辑（默认 true） */
  editable?: boolean;
  /**
   * 内容变更回调（**小文档**的每次编辑触发，父组件写回 store）。
   *
   * 大文档（>256KB）走合帧路径时**不经过这里**：那条路要按"编辑发生时的 tabId"落库，
   * 而 `onChange` 的语义是"写当前活动标签"——切走后再回调会记到别的标签头上。
   * 合帧的落库统一由 `stores/editor.ts` 的 `settlePendingEdit()` 完成
   * （见 PERF-13 与 `setPendingEditSource` 的说明）。
   */
  onChange?: (content: string) => void;
}

/**
 * 已打开文件的编辑器状态缓存（模块级，跨组件实例存活）：
 * 切换标签页时编辑器销毁重建，重建 EditorState 会清空 history（Ctrl+Z 失效）——
 * 缓存 state 复用可保留撤销历史与光标位置。内容被外部修改（watcher 刷新等）
 * 导致与缓存不一致时自动失效重建。上限防止无限增长。
 */
// 双上限：条目数（30）+ 估算字节（256MB，单文件最大 10MB + 撤销历史可能数倍膨胀）
const MAX_CACHED_STATES = 30;
const MAX_CACHE_BYTES = 256 * 1024 * 1024;
let stateCacheBytes = 0;
const stateCache = new Map<string, EditorState>();

/** 写入缓存并维护估算字节（doc 长度 × 2 为 UTF-16 单元，history 按 8 倍系数粗估） */
function stateCacheSet(key: string, st: EditorState): void {
  const prev = stateCache.get(key);
  if (prev !== undefined) {
    stateCacheBytes -= prev.doc.length * 2 * 8;
  }
  const bytes = st.doc.length * 2 * 8;
  stateCacheBytes += bytes;
  stateCache.set(key, st);
  // 先按估算字节、再按条目数淘汰最旧
  while ((stateCacheBytes > MAX_CACHE_BYTES || stateCache.size > MAX_CACHED_STATES) && stateCache.size > 1) {
    const oldestKey = stateCache.keys().next().value;
    if (oldestKey === undefined) break;
    const v = stateCache.get(oldestKey);
    if (v !== undefined) stateCacheBytes -= v.doc.length * 2 * 8;
    stateCache.delete(oldestKey);
  }
}

/** 缓存 key：打开序号（fileOpenSeq）区分会话——文件树重新打开时序号递增 → 缓存未命中、
 * 重建编辑器（撤销历史清空）；标签切换不递增 → 缓存命中、历史保留 */
function cacheKey(filePath: string, editable: boolean, openSeq: number): string {
  return `${editable ? 'e' : 'r'}:${openSeq}:${filePath}`;
}

/**
 * 当前挂载的活动编辑器（全局 Ctrl+Z 转发用）。
 * CodeMirror 的 keymap 只在编辑器聚焦时接收键盘事件——用户切回文件后焦点
 * 常在标签栏/文件树上，Ctrl+Z 无反应（内容未撤销却以为撤销了，dirty 提示"保存"
 * 让人困惑）。焦点在编辑器或输入框时放行（CodeMirror/浏览器自己处理）
 *
 * 仅本文件使用（模块级单例，供全局 Ctrl+Z 转发定位活动编辑器），故不导出。
 */
let activeEditorView: EditorView | null = null;

// ── 双击选中代码块（IDEA 风格，全语言通用） ────────────────

/** 块边界字符：节点以这些符号开头/结尾时视为"块边界"（防双击单词中间误选大块） */
const BLOCK_BOUNDARY_CHARS = new Set(['{', '}', '(', ')', '[', ']', '<', '>', ';']);

/**
 * 双击任意位置 → 尝试选中「以该位置为边界」的最大语法节点（IDEA/HBuilder 行为）：
 * - 双击 { } ( ) [ ] → 代码块/参数列表/数组（节点边界与符号重合）
 * - 双击 ; → 整条语句（Statement.to 在分号处）
 * - 双击 JSX/HTML/XML/Vue 标签的 < > 甚至 </ 的 / → 整个元素
 *   （StartTag 沿同起点扩展为 Element；EndTag 沿同终点扩展为 JSXElement）
 * 不依赖语言类型或字符白名单：任何语言只要语法树节点边界与双击位置重合即可命中；
 * 未命中（单词中间等）返回 false，走默认双击选词。
 */
const selectBracketBlock = EditorView.domEventHandlers({
  dblclick(event: MouseEvent, view: EditorView) {
    const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
    if (pos == null) return false;
    // 双击可能落在符号上或紧邻符号（如 </ 的 /、} 右侧空白），三处边界都尝试
    for (const p of [pos, pos - 1, pos + 1]) {
      const best = findBoundNode(view, p);
      if (!best) continue;
      view.dispatch({ selection: { anchor: best.from, head: best.to } });
      return true; // 阻止默认双击选词
    }
    return false;
  },
});

/** 找与 pos 边界重合的节点（null = 未命中）。
 * 开边界：字符在 pos → 节点 from == pos；闭边界：to 排他 → 字符在 pos 时节点 to == pos + 1。
 * resolve 必须用 side=1（歧义时取"起始于 pos"的节点）——pos 在开括号/标签起点时，
 * side=-1 会取到 pos 之前的节点导致所有开边界（{ < ( [）全部漏掉 */
function findBoundNode(view: EditorView, pos: number) {
  if (pos < 0 || pos > view.state.doc.length) return null;
  const tree = syntaxTree(view.state);
  const resolved = tree.resolve(pos, 1) ?? tree.topNode;
  // 规则1：沿父链记录命中节点——first 为最小命中（{} 边界语义：双击 } 选中块本体），
  // max 为最大命中（< > ; 等取整元素/整条语句；根节点排除，防文档末尾闭符号选中全文）
  let node: typeof resolved | null = resolved;
  let first: typeof resolved | null = null;
  let max: typeof resolved | null = null;
  while (node) {
    if ((node.from === pos || node.to === pos + 1) && node.parent) {
      if (!first) first = node;
      max = node;
    }
    node = node.parent;
  }
  if (!first || first.from >= first.to) return null;
  // 校验命中节点（最小命中）确实以块边界符号开头或结尾：防双击单词中间/文本节点整块误选。
  // 注意校验针对 first 而非最终结果——比较运算符 < 扩展为 a < b 表达式后边界是普通字符
  const fEdgeOpen = view.state.sliceDoc(first.from, first.from + 1);
  const fEdgeClose = view.state.sliceDoc(first.to - 1, first.to);
  if (!BLOCK_BOUNDARY_CHARS.has(fEdgeOpen) && !BLOCK_BOUNDARY_CHARS.has(fEdgeClose)) return null;
  // 规则2：同起点向上扩展（StartTag → Element：双击开始标签的 > 选中整个元素而非仅标签）
  let cur = max!;
  while (cur.parent && cur.parent.parent && cur.parent.from === cur.from) {
    cur = cur.parent;
  }
  // 规则3：{} 边界取最小命中（IDEA：双击 } 选中块本身，而非整个函数/类声明）；
  // 其余（< > ; ( ) [ ]）取最大命中（整元素 / 整条语句 / 参数列表）
  const chosen = (fEdgeOpen === '{' || fEdgeClose === '}') ? first : cur;
  // 规则4：命中节点过小（≤2 字符，如比较运算符 a < b 的 <）→ 向上扩展一层到包含它的表达式
  let result = chosen;
  if (result.to - result.from <= 2 && result.parent) {
    result = result.parent;
  }
  return result;
}

// ── CSS 颜色值色块（VS Code 风格：颜色值后显示色块，仅 CSS/SCSS/LESS） ──

class ColorSwatchWidget extends WidgetType {
  constructor(readonly color: string) {
    super();
  }
  toDOM(): HTMLElement {
    const span = document.createElement('span');
    span.className = 'cm-color-swatch';
    span.style.background = this.color;
    span.title = this.color;
    return span;
  }
  eq(other: ColorSwatchWidget) {
    return other.color === this.color;
  }
  ignoreEvent() {
    return true;
  }
}

/** 常用命名颜色（完整表太大，取高频项；hex / rgb() / hsl() 已覆盖其余） */
const NAMED_COLORS: Record<string, string> = {
  black: '#000', white: '#fff', red: '#f00', green: '#008000', blue: '#00f',
  yellow: '#ff0', orange: '#ffa500', pink: '#ffc0cb', purple: '#800080',
  brown: '#a52a2a', gray: '#808080', grey: '#808080', cyan: '#0ff',
  magenta: '#f0f', lime: '#0f0', navy: '#000080', teal: '#008080',
  silver: '#c0c0c0', gold: '#ffd700', coral: '#ff7f50', transparent: 'transparent',
};

/** 校验 rgb()/hsl() 参数：至少含 3 个数字（宽松容错，无效色不显示色块） */
function isValidColorFunc(raw: string): boolean {
  return (raw.match(/\d+(?:\.\d+)?/g) ?? []).length >= 3;
}

/** 提取颜色值 → 可直接作背景色的 CSS 色串；无法解析返回 null */
function parseColorValue(raw: string): string | null {
  const s = raw.trim();
  if (s.startsWith('#')) return s; // hex（3/4/6/8 位已由正则保证格式）
  if (/^(?:rgba?|hsla?)\(/i.test(s)) return isValidColorFunc(s) ? s : null;
  return NAMED_COLORS[s.toLowerCase()] ?? null;
}

/** hex（3/4/6/8 位）、rgb()/hsl() 函数、命名颜色（命名色匹配后查表过滤） */
const COLOR_RE = /#(?:[0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})\b|\b(?:rgba?|hsla?)\([^)]*\)|\b[a-zA-Z]{2,20}(?=[\s;})]|$)/g;

/** Vue/HTML 的 <style> 块区间（含 lang="scss" 等属性），仅块内启用色块 */
const STYLE_BLOCK_RE = /<style\b[^>]*>([\s\S]*?)<\/style>/gi;

/** 色块上限与文档大小上限：防超大文件编辑时全文档扫描卡顿 */
const MAX_SWATCHES = 1000;
const MAX_SWATCH_DOC = 2 * 1024 * 1024;

/** 在 [from, to) 区间内扫描颜色值，追加色块装饰 */
function scanRange(
  text: string,
  from: number,
  to: number,
  widgets: Range<Decoration>[],
): void {
  COLOR_RE.lastIndex = from;
  let m: RegExpExecArray | null;
  while (widgets.length < MAX_SWATCHES && (m = COLOR_RE.exec(text)) && m.index < to) {
    const color = parseColorValue(m[0]);
    if (!color) continue;
    const end = m.index + m[0].length;
    widgets.push(Decoration.widget({ widget: new ColorSwatchWidget(color), side: 1 }).range(end, end));
  }
}

/**
 * 扫描文档中的颜色值，生成色块装饰。
 * styleBlockOnly（Vue/HTML）：只在 <style> 块内扫描，不误伤 template/script 里的文本颜色
 */
function buildSwatchDeco(doc: Text, styleBlockOnly: boolean): DecorationSet {
  if (doc.length > MAX_SWATCH_DOC) return Decoration.none;
  const widgets: Range<Decoration>[] = [];
  const text = doc.toString();
  if (styleBlockOnly) {
    STYLE_BLOCK_RE.lastIndex = 0;
    let sm: RegExpExecArray | null;
    while (widgets.length < MAX_SWATCHES && (sm = STYLE_BLOCK_RE.exec(text))) {
      const contentStart = sm.index + sm[0].indexOf('>') + 1;
      const contentEnd = sm.index + sm[0].length - '</style>'.length;
      scanRange(text, contentStart, contentEnd, widgets);
    }
  } else {
    scanRange(text, 0, text.length, widgets);
  }
  return Decoration.set(widgets, true);
}

/** 颜色值色块装饰：创建/内容变化时重新解析文档，在颜色值后画色块 */
function makeColorSwatchField(styleBlockOnly: boolean) {
  return StateField.define<DecorationSet>({
    create(state) {
      return buildSwatchDeco(state.doc, styleBlockOnly);
    },
    update(deco, tr) {
      if (!tr.docChanged) return deco.map(tr.changes);
      // 增量预判：改动文本里没有任何颜色线索时直接沿用旧装饰（按变更区间挪位）。
      // 全文 toString + 全文档正则（COLOR_RE 的命名色分支几乎匹配每个标识符）是按键路径上
      // 最贵的一步，而普通打字（字母/数字/回车）根本不可能改变颜色值。
      let maybeColor = false;
      tr.changes.iterChangedRanges((_fromA, _toA, fromB, toB) => {
        if (maybeColor) return;
        const inserted = tr.state.sliceDoc(fromB, toB);
        if (inserted === '' || /[#<]|rgb|hsl|\bstyle\b/i.test(inserted)) maybeColor = true;
      });
      // 删除也可能是删掉颜色值的一部分：保守起来只在"纯插入且无疑似线索"时跳过重扫
      const onlyInsertions = tr.changes.empty || (() => {
        let insertedOnly = true;
        tr.changes.iterChangedRanges((fromA, toA) => { if (toA > fromA) insertedOnly = false; });
        return insertedOnly;
      })();
      if (!maybeColor && onlyInsertions) return deco.map(tr.changes);
      return buildSwatchDeco(tr.state.doc, styleBlockOnly);
    },
    provide: f => EditorView.decorations.from(f),
  });
}

// ── 搜索命中高亮（动态装饰：定位时 dispatch 命中区间） ──
const searchHitEffect = StateEffect.define<{ from: number; to: number }[]>();
const searchHitField = StateField.define({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(searchHitEffect)) {
        const mark = Decoration.mark({ class: 'cm-search-hit' });
        deco = Decoration.set(e.value.map(h => mark.range(h.from, h.to)), true);
      }
    }
    return deco;
  },
  provide: f => EditorView.decorations.from(f),
});

// ── 自定义文件内搜索面板（中文 UI，项目风格） ──

function SearchPanelView({ view }: { view: EditorView }) {
  const [value, setValue] = useState('');
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [regexp, setRegexp] = useState(false);
  const [wholeWord, setWholeWord] = useState(false);
  const [total, setTotal] = useState(0);
  const [current, setCurrent] = useState(0);
  const [invalid, setInvalid] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    // 打开面板即清空上次搜索的 query/高亮：输入框初始为空，不能让上次的匹配残留
    // （CodeMirror 关闭面板不会清 query；注意不能传 null——@codemirror/search 6.7.1
    // 内部 effect.value.create() 对 null 崩溃，须传空 SearchQuery）
    view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: '' })) });
  }, [view]);

  /** 统计匹配总数与当前序号：优先取"包含光标位置的匹配"，否则取光标后的第一个
   * （1-based；getCursor 内部处理大小写/正则/全词。findNext 后光标 head 在匹配末尾，
   * 若按 from>=head 会把当前匹配跳过——先检查 from<=head<=to） */
  const computeMatchPosition = (q: SearchQuery): { total: number; current: number } => {
    let t = 0;
    let inside = 0;
    let after = 0;
    const head = view.state.selection.main.head;
    const cursor = q.getCursor(view.state.doc);
    let match = cursor.next();
    while (!match.done) {
      t++;
      if (inside === 0 && head >= match.value.from && head <= match.value.to) inside = t;
      if (after === 0 && match.value.from >= head) after = t;
      match = cursor.next();
    }
    return { total: t, current: inside || after || t };
  };

  /** 按当前输入与光标位置刷新计数（跳转按钮用：findNext/Prev 移动光标后序号会变） */
  const refreshCount = () => {
    try {
      const { total, current } = computeMatchPosition(new SearchQuery({ search: value, caseSensitive, regexp, wholeWord }));
      setTotal(total);
      setCurrent(current);
    } catch { /* 无效正则：保持显示 */ }
  };

  // 应用搜索：更新 query → 统计匹配与当前序号 → 跳转第一个
  const applySearch = (search: string, cs: boolean, re: boolean, ww: boolean) => {
    // 不用 `= null` 初值：catch 分支直接 return，初值永远读不到（lint 的 no-useless-assignment）
    let q: SearchQuery;
    try {
      q = new SearchQuery({ search, caseSensitive: cs, regexp: re, wholeWord: ww });
      setInvalid(false);
    } catch {
      setInvalid(true); // 正则无效
      setTotal(0);
      setCurrent(0);
      return;
    }
    // 输入清空：显式清除 query（避免空 query 残留高亮；不能传 null，见上）
    if (!search) {
      view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: '' })) });
      setTotal(0);
      setCurrent(0);
      return;
    }
    view.dispatch({ effects: setSearchQuery.of(q) });
    // 先跳转到第一个匹配再统计：光标已在新匹配上，计数与按钮跳转连续（不滞后一个）
    if (search) findNext(view);
    const { total, current } = computeMatchPosition(q);
    setTotal(total);
    setCurrent(current);
  };

  const handleInput = (v: string) => {
    setValue(v);
    applySearch(v, caseSensitive, regexp, wholeWord);
  };
  const toggleCase = () => { const v = !caseSensitive; setCaseSensitive(v); applySearch(value, v, regexp, wholeWord); };
  const toggleRegexp = () => { const v = !regexp; setRegexp(v); applySearch(value, caseSensitive, v, wholeWord); };
  const toggleWholeWord = () => { const v = !wholeWord; setWholeWord(v); applySearch(value, caseSensitive, regexp, v); };
  const goPrev = () => { if (!value) return; findPrevious(view); refreshCount(); };
  const goNext = () => { if (!value) return; findNext(view); refreshCount(); };

  const btnCls = "flex-shrink-0 w-[22px] h-[22px] flex items-center justify-center rounded hover:bg-[#2c313c] text-[#abb2bf] transition-colors disabled:opacity-40 disabled:hover:bg-transparent";
  const toggleCls = (active: boolean) =>
    `flex-shrink-0 h-[22px] px-1.5 text-[11px] font-medium rounded transition-colors ${
      active ? 'text-[#4f8cff] bg-[#4f8cff]/10' : 'text-[#8a93a5] hover:bg-[#2c313c] hover:text-[#abb2bf]'
    }`;

  return (
    <div className="flex items-center gap-1.5 px-3 h-[36px] bg-[#21252b] border-b border-[#383c47]">
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.4" className="text-[#8a93a5] flex-shrink-0">
        <circle cx="5" cy="5" r="3.5"/><line x1="7.8" y1="7.8" x2="10.5" y2="10.5"/>
      </svg>
      <input
        ref={inputRef}
        className={`w-[240px] px-2 py-1 text-[12px] bg-[#1e222a] border rounded text-[#abb2bf] font-mono focus:outline-none transition-colors ${
          invalid ? 'border-[#e06c75]' : 'border-[#3a3f4b] focus:border-[#4f8cff]'
        }`}
        placeholder="查找"
        value={value}
        onChange={e => handleInput(e.target.value)}
        onKeyDown={e => {
          // 中文输入法回车确认候选词不应触发"跳到下一处"（isComposing 判断在 isSubmitEnter 内）
          if (isSubmitEnter(e)) { if (e.shiftKey) goPrev(); else goNext(); }
          if (e.key === 'Escape') { closeSearchPanel(view); view.focus(); }
        }}
      />
      <span className="flex-shrink-0 w-[44px] text-right text-[11px] text-[#5c6370] font-mono">
        {total > 0 ? `${current}/${total}` : ''}
      </span>
      <button className={btnCls} title="上一个（Shift+Enter）" onClick={goPrev} disabled={!value}>
        <svg width="11" height="11" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"><polyline points="1,6 5,2 9,6"/></svg>
      </button>
      <button className={btnCls} title="下一个（Enter）" onClick={goNext} disabled={!value}>
        <svg width="11" height="11" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"><polyline points="1,4 5,8 9,4"/></svg>
      </button>
      <div className="w-px h-[16px] bg-[#383c47] mx-1 flex-shrink-0" />
      <button className={toggleCls(caseSensitive)} title="区分大小写" onClick={toggleCase}>Aa</button>
      <button className={toggleCls(regexp)} title="正则表达式" onClick={toggleRegexp}>.*</button>
      <button className={toggleCls(wholeWord)} title="全词匹配" onClick={toggleWholeWord}>全词</button>
      <div className="flex-1" />
      <button
        className="flex-shrink-0 h-[22px] px-2 text-[11px] text-[#8a93a5] border border-[#3a3f4b] rounded hover:text-[#abb2bf] hover:border-[#5c6370] transition-colors"
        title="关闭（Esc）"
        onClick={() => { closeSearchPanel(view); view.focus(); }}
      >关闭</button>
    </div>
  );
}

/** 自定义搜索面板（替代默认英文面板，React 渲染到 CodeMirror Panel 容器） */
function createSearchPanel(view: EditorView): Panel {
  const dom = document.createElement('div');
  const root = createRoot(dom);
  root.render(<SearchPanelView view={view} />);
  return {
    dom,
    top: true,
    destroy: () => { root.unmount(); },
  };
}

/** 光标是否在 HTML 注释节点内（<!-- -->；主树节点，script/style 的内嵌树不会误判） */
function isInsideHtmlComment(view: EditorView, pos: number): boolean {
  const resolved = syntaxTree(view.state).resolveInner(pos, 1);
  let cur: typeof resolved | null = resolved;
  while (cur && !cur.type.isTop) {
    if (cur.name === 'Comment') return true;
    cur = cur.parent;
  }
  return false;
}

/**
 * Vue 模板注释修复：@codemirror/lang-vue 0.1.3 的嵌套解析结构导致
 * languageDataAt("commentTokens") 在 template 普通文本区域查不到配置（isActiveAt 恒 false），
 * 标准 toggleComment 直接失效。自定义 Mod-/ 分支：
 * - template 普通文本 / HTML 注释节点 → toggleBlockCommentByLine（<!-- -->，按整行判断，
 *   取消可靠；toggleComment 对光标在注释内部时按选区范围检测会失效）
 * - script/style 区域（javascript/css 有自带配置且不在注释节点）→ 标准 toggleComment（行注释或块注释）
 */
function vueCommentToggle(view: EditorView): boolean {
  const pos = view.state.selection.main.head;
  const hasConfig = view.state.languageDataAt('commentTokens', pos, 1).length > 0;
  if (hasConfig && !isInsideHtmlComment(view, pos)) {
    return toggleComment(view);
  }
  return toggleBlockCommentByLine(view);
}

// ── Vue 的 <style lang="scss"/"less"> 块解析（lang-html 缺陷修复） ─────

/**
 * lang-html 的 style 块混合解析只认 lang="css" 或无 lang（defaultNesting 硬编码），
 * lang="scss"/"less" 的块完全不解析 → 无高亮/无缩进/无补全。
 * 在 lang-vue 的 wrap 链上追加 StyleElement 处理（wrap 链式：返回 null 回落内置逻辑，
 * 不影响 template 绑定表达式、script 块等原有解析）
 */
const vueScssMixed = parseMixed((nodeRef, input: Input) => {
  // lezer html 树里 style 块内容节点名是 StyleText（html 内置混合按节点类型 id 匹配）
  const node = nodeRef.node as SyntaxNode;
  if (node.name !== 'StyleText') return null;
  // 读 <style> 开始标签的属性（与 @lezer/html getAttrs 同法）
  const openTag = node.parent?.firstChild;
  if (!openTag) return null;
  const attrs: Record<string, string> = {};
  for (const att of openTag.getChildren('Attribute')) {
    const name = att.getChild('AttributeName');
    const value = att.getChild('AttributeValue') ?? att.getChild('UnquotedAttributeValue');
    if (name) {
      attrs[input.read(name.from, name.to)] = !value ? ''
        : value.name === 'AttributeValue' ? input.read(value.from + 1, value.to - 1) : input.read(value.from, value.to);
    }
  }
  // scss/less 用 CSS 解析器硬解析（嵌套选择器为 error 节点，但属性名/值/数字正常高亮）
  if (attrs.lang === 'scss' || attrs.lang === 'less') return { parser: cssLanguage.parser, bracketed: true };
  return null;
});

/** legacy-modes 桥接：无官方 @codemirror/lang-* 的常用语言用 StreamLanguage 包装 */
function legacyLang(mode: StreamParser<unknown>): Language {
  return StreamLanguage.define(mode);
}

/** 根据文件扩展名获取语言支持（同步版，仅用于 JS 系列：最常用且体量小） */
function getLanguageExtension(filePath: string) {
  const ext = getExtension(filePath, { lower: true });
  if (ext === 'js' || ext === 'jsx' || ext === 'mjs' || ext === 'cjs') return [javascript()];
  if (ext === 'ts' || ext === 'tsx') return [javascript({ typescript: true, jsx: ext === 'tsx' })];
  return [];
}

/**
 * 语言包按需加载表：扩展名 → 加载器。
 *
 * 为什么改成动态导入：原来 30 个语言包全部静态导入（17 个 `@codemirror/lang-*` +
 * 11 个 legacy-modes），冷启动必须解析并执行**全部**语言包，而一次会话通常只用一两种。
 * 拆成动态导入后，主包只留核心（CodeMirror 运行时 + 本次打开文件的语言包）。
 */
const langLoaders: Record<string, () => Promise<Language | LanguageSupport>> = {
  // 注意：js/jsx/ts/tsx/mjs/cjs **不在这里**——它们由 `getLanguageExtension` 同步装配
  // （最常用的几种，且已在主包里）。此前这里也登记了一份动态导入，模块同时被静态与动态
  // 引用 → Vite 警告"dynamic import will not move module into another chunk"（动态那半
  // 白写），且装配完成后还会用一个等价实例再 reconfigure 一次。同步路径已覆盖，故摘除。
  //
  // css/scss 不能用动态导入：该模块被 `vueScssMixed` 静态引用，无论怎么写都会留在主包，
  // 动态导入同样拆不出去。直接用已导入的工厂，省掉一次无意义的 chunk 边界请求。
  py: () => import('@codemirror/lang-python').then(m => m.python()),
  java: () => import('@codemirror/lang-java').then(m => m.java()),
  class: () => import('@codemirror/lang-java').then(m => m.java()), // .class 用 Java 高亮（反编译源码）
  css: () => Promise.resolve(css()),
  scss: () => Promise.resolve(css()), // 无官方 lang-scss，CSS 解析器近似
  less: () => import('@codemirror/lang-less').then(m => m.less()),
  sass: () => import('@codemirror/lang-sass').then(m => m.sass()),
  styl: () => import('@codemirror/legacy-modes/mode/stylus').then(m => legacyLang(m.stylus)),
  html: () => import('@codemirror/lang-html').then(m => m.html()),
  htm: () => import('@codemirror/lang-html').then(m => m.html()),
  json: () => import('@codemirror/lang-json').then(m => m.json()),
  jsonc: () => import('@codemirror/lang-json').then(m => m.json()),
  json5: () => import('@codemirror/lang-json').then(m => m.json()),
  geojson: () => import('@codemirror/lang-json').then(m => m.json()),
  md: () => import('@codemirror/lang-markdown').then(m => m.markdown()),
  rs: () => import('@codemirror/lang-rust').then(m => m.rust()),
  go: () => import('@codemirror/lang-go').then(m => m.go()),
  sql: () => import('@codemirror/lang-sql').then(m => m.sql()),
  vue: async () => {
    const { vue } = await import('@codemirror/lang-vue');
    const base = vue();
    // 追加 style lang="scss"/"less" 块解析（见 vueScssMixed）。
    //
    // 这里**运行期判定**而不是 `as LRLanguage` 断言（CQ-21）：`.configure()` 只存在于
    // `LRLanguage`，当前只因 lang-vue 恰好基于它才成立——依赖升级把 `base.language`
    // 换成别的 Language 实现时，断言会安静放行到 `.configure is not a function` 的
    // TypeError，且只在打开 .vue 时触发（编译期零提示）。判定失败就退回原样，
    // 编辑器的语法高亮退化，而不是整块炸掉。
    return base.language instanceof LRLanguage
      ? new LanguageSupport(base.language.configure({ wrap: vueScssMixed }), base.support)
      : base;
  },
  xml: () => import('@codemirror/lang-xml').then(m => m.xml()),
  xhtml: () => import('@codemirror/lang-xml').then(m => m.xml()),
  svg: () => import('@codemirror/lang-xml').then(m => m.xml()),
  // C/C++
  c: () => import('@codemirror/lang-cpp').then(m => m.cpp()),
  h: () => import('@codemirror/lang-cpp').then(m => m.cpp()),
  cc: () => import('@codemirror/lang-cpp').then(m => m.cpp()),
  cpp: () => import('@codemirror/lang-cpp').then(m => m.cpp()),
  cxx: () => import('@codemirror/lang-cpp').then(m => m.cpp()),
  hpp: () => import('@codemirror/lang-cpp').then(m => m.cpp()),
  // YAML / PHP
  yml: () => import('@codemirror/lang-yaml').then(m => m.yaml()),
  yaml: () => import('@codemirror/lang-yaml').then(m => m.yaml()),
  php: () => import('@codemirror/lang-php').then(m => m.php()),
  // 脚本（legacy-modes）
  sh: () => import('@codemirror/legacy-modes/mode/shell').then(m => legacyLang(m.shell)),
  bash: () => import('@codemirror/legacy-modes/mode/shell').then(m => legacyLang(m.shell)),
  zsh: () => import('@codemirror/legacy-modes/mode/shell').then(m => legacyLang(m.shell)),
  // 无 batch 模式，PowerShell 近似
  bat: () => import('@codemirror/legacy-modes/mode/powershell').then(m => legacyLang(m.powerShell)),
  cmd: () => import('@codemirror/legacy-modes/mode/powershell').then(m => legacyLang(m.powerShell)),
  ps1: () => import('@codemirror/legacy-modes/mode/powershell').then(m => legacyLang(m.powerShell)),
  rb: () => import('@codemirror/legacy-modes/mode/ruby').then(m => legacyLang(m.ruby)),
  erb: () => import('@codemirror/legacy-modes/mode/ruby').then(m => legacyLang(m.ruby)), // ERB，Ruby 近似
  // 其他（legacy-modes）
  dockerfile: () => import('@codemirror/legacy-modes/mode/dockerfile').then(m => legacyLang(m.dockerFile)),
  makefile: () => import('@codemirror/legacy-modes/mode/shell').then(m => legacyLang(m.shell)), // Shell 近似
  swift: () => import('@codemirror/legacy-modes/mode/swift').then(m => legacyLang(m.swift)),
  lua: () => import('@codemirror/legacy-modes/mode/lua').then(m => legacyLang(m.lua)),
  toml: () => import('@codemirror/legacy-modes/mode/toml').then(m => legacyLang(m.toml)),
  proto: () => import('@codemirror/legacy-modes/mode/protobuf').then(m => legacyLang(m.protobuf)),
  cs: () => import('@codemirror/legacy-modes/mode/clike').then(m => legacyLang(m.csharp)),
  kt: () => import('@codemirror/legacy-modes/mode/clike').then(m => legacyLang(m.kotlin)),
  kts: () => import('@codemirror/legacy-modes/mode/clike').then(m => legacyLang(m.kotlin)),
  ini: () => import('@codemirror/legacy-modes/mode/properties').then(m => legacyLang(m.properties)),
  conf: () => import('@codemirror/legacy-modes/mode/properties').then(m => legacyLang(m.properties)),
  properties: () => import('@codemirror/legacy-modes/mode/properties').then(m => legacyLang(m.properties)),
  env: () => import('@codemirror/legacy-modes/mode/properties').then(m => legacyLang(m.properties)),
  gitignore: () => import('@codemirror/legacy-modes/mode/properties').then(m => legacyLang(m.properties)),
};

/** 语言加载缓存：同一扩展名只加载一次（Promise 也缓存，避免并发重复请求） */
const languageCache = new Map<string, Promise<Language | LanguageSupport | null>>();

/** 按扩展名加载语言包；失败一律降级为"无高亮"（不让编辑器创建失败） */
function loadLanguage(filePath: string): Promise<Language | LanguageSupport | null> {
  const ext = getExtension(filePath, { lower: true });
  const loader = langLoaders[ext];
  if (!loader) return Promise.resolve(null);
  const cached = languageCache.get(ext);
  if (cached) return cached;
  const p = loader().catch(e => {
    // 只留控制台：已按设计降级（无高亮仍可读可编辑），弹 toast 会在每次打开这类文件时重复打扰
    console.error(`加载语言包失败（${ext}，已降级为无高亮）:`, e);
    return null;
  });
  languageCache.set(ext, p);
  return p;
}

/** 选中匹配的滚动条标记：匹配行集合 + 总行数（按行聚合） */
interface MatchMarks {
  lines: Set<number>;
  total: number;
}

/** 滚动条标记的文档大小上限（防超大文件选区扫描卡顿） */
const MAX_MARK_DOC = 5 * 1024 * 1024;

/** 选区匹配行扫描的防抖窗口（ms）：滚动条标记非瞬时反馈，攒够一次即可 */
const MARK_SCAN_DEBOUNCE_MS = 120;

/**
 * 小文档直接写回 store 的长度上限（PERF-13）。
 *
 * 为什么要分档：`doc.toString()` 每次按键物化整篇文档，实测 2k 行/100KB 是 0.15ms、
 * 10MB 是 4.5ms（并按 100MB/s 产生垃圾触发 major GC）。256KB 以下的成本在 0.1ms 量级，
 * 而"store 里永远是最新内容"能省掉一整套 flush 时机管理——**绝大部分文件走的就是这条
 * 零风险路径**，只有真正大的文件才需要合帧。
 */
const INSTANT_EMIT_MAX_LENGTH = 256 * 1024;

/**
 * 大文档合帧窗口（ms）。取 200ms：比人手速（约 10 键/秒）长，连打时只在停顿处物化一次；
 * 又比"用户察觉卡顿"短——真正决定不丢内容的是 `settlePendingEdit` 的读取点（保存/切标签/
 * 关标签/卸载都会先结算），这个定时器只是让 store 里的内容不至于长时间落后。
 */
const EDIT_COALESCE_MS = 200;

/** 语言槽：语言包按需加载（见 langLoaders），加载完成后用它把语言装进已有编辑器 */
const languageCompartment = new Compartment();

/** 创建 CodeMirror 编辑器状态（含语言支持和主题） */
function createEditorState(
  content: string,
  filePath: string,
  editable: boolean,
  /**
   * 文档变更回调。**交出去的是 EditorState 而不是已物化的字符串**（PERF-13）：
   * 物化整篇文档的代价随文档大小线性增长（实测 10MB → 4.5ms/键），要不要现在物化、
   * 还是攒一会儿再物化，只有调用方（它知道大小与保存时机）能决定。
   */
  onDocChange: (state: EditorState) => void,
  onMatchMarks: (marks: MatchMarks | null) => void,
) {
  // 上次扫描的选中文本：updateListener 据此跳过重复全文档扫描
  let lastMarkText: string | null = null;
  /** 扫描防抖计时器：选区/文档连续变化时只在停下来之后扫一次 */
  let markScanTimer: ReturnType<typeof setTimeout> | null = null;

  const extensions = [
    lineNumbers(),
    highlightActiveLine(),
    highlightActiveLineGutter(),
    indentOnInput(),
    indentationMarkers(),
    bracketMatching(),
    selectBracketBlock,
    foldGutter(),
    history(),
    // 文件内搜索（Ctrl+F 打开查找面板，仅编辑器聚焦时生效）
    search({ createPanel: createSearchPanel }),
    // 选中文字 → 高亮文档中所有相同匹配（VS Code 风格，选区变化自动更新）
    highlightSelectionMatches({ minSelectionLength: 2 }),
    // 选区变化 → 计算匹配行集合（滚动条标记用，按行聚合：一行多个匹配只算一个）。
    // 性能护栏：文档 >5MB 跳过；选中文本未变且文档未变时复用上次结果（原实现每次
    // 选区移动都全文档逐行扫描，数万行文件光标拖动即卡顿）；命中 500 行提前终止
    //（MatchMarksOverlay 最多也只渲染 500 个，多余扫描纯浪费）
    EditorView.updateListener.of(update => {
      if (!update.selectionSet && !update.docChanged) return;
      const sel = update.state.selection.main;
      const doc = update.state.doc;
      if (
        sel.empty || sel.from === sel.to ||
        sel.to - sel.from < 2 || sel.to - sel.from > 200 ||
        doc.length > MAX_MARK_DOC
      ) {
        lastMarkText = null;
        onMatchMarks(null);
        return;
      }
      const text = update.state.sliceDoc(sel.from, sel.to);
      if (text.includes('\n')) {
        lastMarkText = null;
        onMatchMarks(null);
        return;
      }
      if (!update.docChanged && lastMarkText === text) return; // 内容没变，标记仍有效
      lastMarkText = text;
      // 扫描防抖：有选区时每次按键/每次指针移动都会到这里，全文档逐行 includes（数千行文件
      // 即数千次行物化）不该每秒跑十几遍。滚动条标记本来就不是瞬时反馈，120ms 后扫一次即可。
      if (markScanTimer !== null) clearTimeout(markScanTimer);
      markScanTimer = setTimeout(() => {
        markScanTimer = null;
        const lines = new Set<number>();
        for (let n = 1; n <= doc.lines && lines.size < 500; n++) {
          if (doc.line(n).text.includes(text)) lines.add(n);
        }
        onMatchMarks({ lines, total: doc.lines });
      }, MARK_SCAN_DEBOUNCE_MS);
    }),
    searchHitField,
    syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
    oneDark,
    keymap.of([
      ...defaultKeymap,
      ...historyKeymap,
      ...foldKeymap,
      // historyKeymap 的 redo 只在 linux 绑了 Ctrl-Shift-z（避免与系统/浏览器冲突），
      // Windows 上需显式补绑定（Mod=Ctrl）
      { key: 'Mod-Shift-z', run: redo, preventDefault: true },
      // 文件内搜索：显式绑定保证生效（search() 自带绑定带 scope 且优先级靠后）。
      // Ctrl+F 打开/聚焦搜索框并全选其内容（VS Code 行为：面板已打开时再按也全选，直接输入即替换）。
      // rAF 等 React 渲染出输入框（createRoot 渲染是异步调度）
      { key: 'Mod-f', run: (view) => {
        openSearchPanel(view);
        requestAnimationFrame(() => {
          // instanceof 运行时检查：querySelector 结果不保证是 input（占位符可能被改）
          const el = view.dom.querySelector('input[placeholder="查找"]');
          if (el instanceof HTMLInputElement) {
            el.focus();
            el.select();
          }
        });
        return true;
      }, preventDefault: true },
      { key: 'F3', run: findNext, shift: findPrevious, preventDefault: true },
      { key: 'Mod-g', run: findNext, shift: findPrevious, preventDefault: true },
      // Vue 模板注释修复（lang-vue 缺陷）：需先于语言包 keymap 注册才优先匹配；
      // template 区域 HTML 注释，script/style 区域返回 false 交由语言包处理
      ...(filePath.toLowerCase().endsWith('.vue') ? [{ key: 'Mod-/', run: vueCommentToggle }] : []),
    ]),
    // 语言槽：JS 系列同步装（最常用且体量小），其余先留空、由 loadLanguage 异步 reconfigure
    languageCompartment.of(getLanguageExtension(filePath)),
    // 颜色值色块（VS Code 风格）：CSS 类文件全文；Vue/HTML 仅 <style> 块内
    ...(() => {
      const ext = getExtension(filePath, { lower: true });
      if (['css', 'scss', 'less', 'sass', 'styl'].includes(ext)) return [makeColorSwatchField(false)];
      if (ext === 'vue' || ext === 'html' || ext === 'htm') return [makeColorSwatchField(true)];
      return [];
    })(),
    EditorView.theme({
      '&': {
        height: '100%',
        // 缩进对齐线颜色：普通线比背景（#282c34）略亮，活动块线用 accent 色系
        '--indent-marker-bg-color': '#3a3f4b',
        '--indent-marker-active-bg-color': '#4a5a7a',
      },
      // 原生滚动条样式化（CSS 伪元素，浏览器处理布局，无重叠/遮挡问题）
      '.cm-scroller': {
        overflow: 'auto',
        scrollbarWidth: 'thin',                              // Firefox
        scrollbarColor: '#4a5161 #21252b',                   // Firefox: thumb track
      },
      '.cm-scroller::-webkit-scrollbar': { width: 14, height: 14 },
      '.cm-scroller::-webkit-scrollbar-track': { background: '#21252b' },
      '.cm-scroller::-webkit-scrollbar-thumb': {
        background: '#4a5161',
        borderRadius: 7,
        border: '2px solid #21252b',
        '&:hover': { background: '#5c6370' },
      },
      '.cm-scroller::-webkit-scrollbar-corner': { background: '#21252b' },
      '.cm-content': {
        fontFamily: "'JetBrains Mono', 'Fira Code', Consolas, monospace",
        fontSize: '14px',
      },
      '.cm-gutters': {
        backgroundColor: '#282c34',
        borderRight: '1px solid #383c47',
        color: '#5c6370',
      },
      '.cm-activeLineGutter': { backgroundColor: '#2c313c' },
      '.cm-activeLine': { backgroundColor: '#2c313c' },
      '.cm-foldGutter': { color: '#5c6370' },
      // 搜索面板层级：默认 300 过高，会盖住标签右键菜单等页面级弹窗
      '.cm-panels': { zIndex: 50 },
      // 搜索结果命中高亮（背景色）
      '.cm-search-hit': {
        backgroundColor: 'rgba(240, 190, 60, 0.22)',
        borderRadius: '2px',
      },
      // 选中文字的其他相同匹配（蓝色，区别于搜索的黄色）
      '.cm-selectionMatch': {
        backgroundColor: 'rgba(80, 140, 255, 0.18)',
        borderRadius: '2px',
      },
      // 匹配高亮：当前匹配亮、其余匹配暗
      '.cm-searchMatch': {
        backgroundColor: 'rgba(240, 190, 60, 0.15)',
        outline: '1px solid rgba(240, 190, 60, 0.45)',
      },
      '.cm-searchMatch-selected': {
        backgroundColor: 'rgba(240, 190, 60, 0.35)',
        outline: '1px solid rgba(240, 190, 60, 0.9)',
      },
      // 选区背景（双击选中/拖选）——最醒目
      '& .cm-selectionBackground, &.cm-focused .cm-selectionBackground': {
        backgroundColor: 'rgba(55, 75, 115, 0.55) !important',
      },
      // 括号/标签匹配高亮（单击标签时）——柔和提示，低于选区
      '& .cm-matchingBracket, & .cm-matchingTag, & .cm-selectionMatch': {
        backgroundColor: 'rgba(80, 140, 255, 0.12) !important',
        outline: '1px solid rgba(80, 140, 255, 0.25)',
      },
      // 非匹配侧的括号（失配提示）
      '& .cm-nonmatchingBracket': {
        backgroundColor: 'rgba(255, 80, 80, 0.2)',
        outline: '1px solid rgba(255, 80, 80, 0.5)',
      },
      // CSS 颜色值色块（值后的小方块）
      '.cm-color-swatch': {
        display: 'inline-block',
        width: '10px',
        height: '10px',
        borderRadius: '2px',
        border: '1px solid rgba(255, 255, 255, 0.35)',
        marginLeft: '4px',
        verticalAlign: 'middle',
        cursor: 'pointer',
      },
    }),
  ];

  if (editable) {
    extensions.push(
      // 变更监听：把新 state 交给调用方决定"现在物化还是合帧"（见 onDocChange 说明）
      EditorView.updateListener.of(update => {
        if (update.docChanged) onDocChange(update.state);
      }),
      // Ctrl/Cmd+S 保存当前文件
      keymap.of([{
        key: 'Mod-s',
        run: () => { void saveActiveFile(); return true; },
      }]),
    );
  } else {
    // 只读：用 readOnly 而非 editable(false)——前者保留焦点/光标/选中复制，仅禁止编辑
    extensions.push(EditorState.readOnly.of(true));
  }

  return EditorState.create({ doc: content, extensions });
}

/**
 * 选中匹配标记 overlay：在垂直滚动条轨道上显示蓝色小圆点，
 * 标记文档中与当前选中文字相同的行。pointer-events-none 不影响滚动条交互。
 *
 * `memo` + `useMemo`：父组件（CodeViewer → ProjectDetail 子树）每次重渲染都会带着同一个
 * `matchMarks` 引用进来，原来会白白重建数组并 reconcile 最多 500 个绝对定位 div。
 */
const MatchMarksOverlay = memo(function MatchMarksOverlay({ matchMarks }: { matchMarks: MatchMarks | null }) {
  const tops = useMemo(
    () => (matchMarks ? [...matchMarks.lines].slice(0, 500) : []),
    [matchMarks],
  );
  if (!matchMarks || tops.length === 0) return null;
  return (
    <div className="absolute right-0 top-0 bottom-0 w-[14px] pointer-events-none z-10">
      {tops.map(n => (
        <div
          key={n}
          className="absolute w-[6px] h-[2px] rounded-full bg-[#4f8cff]"
          style={{ top: `${(n - 0.5) / matchMarks.total * 100}%`, right: 3 }}
        />
      ))}
    </div>
  );
});

export function CodeViewer({ filePath, editable = true, onChange }: CodeViewerProps) {
  const editorRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  /** 选中匹配的滚动条标记（选区变化时由 updateListener 计算） */
  const [matchMarks, setMatchMarks] = useState<MatchMarks | null>(null);
  /**
   * 直接订阅 store 里的当前内容，而不是由父级传 prop。
   *
   * 为什么：编辑时 `fileContent` 每个按键都会更新（那是编辑器自己的回声），若由
   * `ProjectDetail` 订阅再传下来，**整棵 ProjectDetail 子树**（标签栏 + 编辑器 + 全部服务卡 +
   * 模板卡 + 两套 dnd context）都会跟着重渲染。订阅放进本组件后，重渲染只发生在编辑器自身。
   * `contentRef` 与下面的同步 effect 都用这份值，语义与之前的 prop 完全一致。
   */
  const content = useEditorStore(s => s.fileContent) ?? '';
  // 回调与最新内容存 ref：编辑器只在 filePath 变化时重建，
  // 编辑中 content 回写（onChange → store）不触发重建，避免光标/焦点丢失
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const contentRef = useRef(content);
  contentRef.current = content;
  /** 最近一次由本编辑器 emit 出去的内容：用于把"自己刚输入的回声"与"外部内容变化"区分开 */
  const lastEmittedRef = useRef<string | null>(null);
  /** 大文档合帧：待物化的最新编辑器状态（null = 没有未写回的改动） */
  const pendingStateRef = useRef<EditorState | null>(null);
  /** 合帧定时器：空闲 200ms 后把待物化内容写回 store */
  // 用 window.setTimeout（返回 number）：与 @types/node 的 Timeout 区分开，浏览器环境恒为 number
  const settleTimerRef = useRef<number | null>(null);

  const locate = useEditorStore(s => s.locate);
  const clearLocate = useEditorStore(s => s.clearLocate);
  const hitSeq = useEditorStore(s => s.hitSeq);
  /** 文件树重新打开序号：递增 = 新会话（重建编辑器清空撤销历史） */
  const openSeq = useEditorStore(s => s.fileOpenSeq[filePath] ?? 0);

  useEffect(() => {
    if (!editorRef.current) return;

    viewRef.current?.destroy();
    const key = cacheKey(filePath, editable, openSeq);
    const currentContent = contentRef.current;
    const cached = stateCache.get(key);
    let state: EditorState;
    if (cached && cached.doc.toString() === currentContent) {
      // 缓存命中且内容未被外部修改 → 复用（保留撤销历史与光标位置）
      state = cached;
    } else {
      // 内容变化（外部刷新/首次打开）→ 重建并更新缓存
      state = createEditorState(
        currentContent,
        filePath,
        editable,
        (newState) => {
          const text = newState.doc.toString();
          // EditorState 不可变：每次编辑产生新 state 对象，缓存里的引用会过期
          // （doc 对比失败 → 切换回来重建 → 撤销历史丢失）。编辑后把最新
          // state 写回缓存，切换回来 doc 对比命中、Ctrl+Z 历史保留
          stateCacheSet(key, newState);
          if (text.length <= INSTANT_EMIT_MAX_LENGTH) {
            // 小文档（绝大多数）：照旧每键写回 store。物化 256KB 的成本在 0.1ms 量级，
            // 而同步写回意味着"store 里永远是最新内容"——没有 flush 时机的负担
            lastEmittedRef.current = text;
            onChangeRef.current?.(text);
            return;
          }
          // 大文档：不在这里物化（PERF-13）。把这份 state 交给 store 的合帧槽，
          // 由它在本组件请求时（保存/切标签/关标签/卸载）或 200ms 后物化
          pendingStateRef.current = newState;
          if (settleTimerRef.current === null) {
            settleTimerRef.current = window.setTimeout(() => {
              settleTimerRef.current = null;
              settlePendingEdit();
            }, EDIT_COALESCE_MS);
          }
        },
        setMatchMarks,
      );
      stateCacheSet(key, state);
    }

    viewRef.current = new EditorView({
      state,
      parent: editorRef.current,
    });
    activeEditorView = viewRef.current;

    // 把"未合帧的编辑"登记到 store（PERF-13）：登记的是 (tabId, 取文本) 一对，
    // 于是即便结算发生在用户切走之后，内容也会落到**当时那个标签**头上。
    // 没有待物化内容时 take() 返回 null，登记本身零成本。
    const tabId = useEditorStore.getState().tabs.find(t => t.path === filePath)?.id;
    if (tabId) {
      setPendingEditSource(tabId, () => {
        const pending = pendingStateRef.current;
        if (!pending) return null;
        pendingStateRef.current = null;
        const text = pending.doc.toString();
        // 与同步路径一致：把它记为"自己刚发出去的内容"，外部同步 effect 才不会
        // 把这次写回当成"外部变化"再覆盖一遍编辑器
        lastEmittedRef.current = text;
        return text;
      });
    }

    // 语言包按需加载：拿到后把语言装进这个编辑器（缓存命中的 state 同样重装一次，
    // 覆盖"上次加载未完成就切走"的情况）。销毁后到达的加载结果由 view 身份校验拦掉。
    const view = viewRef.current;
    void loadLanguage(filePath).then(lang => {
      if (!lang || viewRef.current !== view) return;
      try {
        view.dispatch({ effects: languageCompartment.reconfigure(lang) });
      } catch (e) {
        // 同上：降级路径，用户可见的结果是「没有高亮」，不需要额外提示
        console.error('装配语言包失败（已降级为无高亮）:', e);
      }
    });

    return () => {
      // 卸载/换文件前必须结算：合帧窗口里的改动还只活在这个编辑器里，
      // 一旦 destroy 就再无第二份（存储侧登记槽也要清掉，避免指向已销毁的 state）
      if (settleTimerRef.current !== null) {
        clearTimeout(settleTimerRef.current);
        settleTimerRef.current = null;
      }
      settlePendingEdit();
      clearPendingEditSource();
      if (activeEditorView === viewRef.current) activeEditorView = null;
      viewRef.current?.destroy();
    };
  }, [filePath, editable, openSeq]);

  /**
   * 外部内容晚到 → 覆盖式同步文档。
   *
   * 编辑器只在 filePath/editable/openSeq 变化时消费 `content`（编辑中不重建，避免丢光标与撤销栈），
   * 但切换标签时若内容缓存未命中，store 会先切 activeTabId、读盘完成后才 setFileContent：
   * 那一次 await 期间编辑器已按新的 filePath 建好，文档里却是**上一个文件的内容**，
   * 此后 content 变化不再触发任何更新 —— 用户看到的是错文件内容，敲一个键就会把它
   * 记到当前标签的草稿上，Ctrl+S 直接写进当前文件。这里补上显式同步。
   */
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    // 先把未合帧的编辑结算掉（PERF-13）：否则下面会把"用户刚敲的内容"当成"外部变化"
    // 覆盖掉（大文档合帧窗口内，store 里的 content 比编辑器旧）
    settlePendingEdit();
    // 自己刚输入的回声先判：store 回写的就是编辑器内容，此时 `content` 与文档必然相等，
    // 无需再 `doc.toString()`（全文档序列化）。原实现把这个便宜的判断放在了**昂贵操作之后**，
    // 于是每次按键都要白白物化一次整篇文档。
    if (content === lastEmittedRef.current) return;
    const doc = view.state.doc.toString();
    if (doc === content) return;
    view.dispatch({ changes: { from: 0, to: doc.length, insert: content } });
    // 缓存同步更新：否则切走再切回会因 doc 与内容不一致而重建（撤销历史丢失）
    stateCacheSet(cacheKey(filePath, editable, openSeq), view.state);
  }, [content, filePath, editable, openSeq]);

  // 全局 Ctrl+F：焦点在搜索框 → 全选其内容（keymap 收不到搜索框的按键——
  // 输入框在 .cm-editor 内但在 .cm-content 外，再按 Ctrl+F 也全选，直接输入即替换）；
  // 焦点不在 .cm-editor 内 → 吞掉事件（避免触发浏览器查找）；编辑器聚焦时放行（keymap 处理）
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey || e.key.toLowerCase() !== 'f') return;
      const el = document.activeElement;
      if (el instanceof HTMLInputElement && el.closest('.cm-editor')) {
        e.preventDefault();
        e.stopPropagation();
        el.select();
      } else if (el && !(el instanceof Element && el.closest('.cm-editor'))) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    document.addEventListener('keydown', handler, true);
    return () => document.removeEventListener('keydown', handler, true);
  }, []);

  // 全局 Ctrl+Z / Ctrl+Shift+Z 转发（焦点不在编辑器/输入框时）：
  // 切回文件后焦点常在标签栏或文件树，编辑器 keymap 收不到按键——捕获阶段
  // 转发到活动编辑器，撤销/重做照常生效（dirty 状态也随之正确更新）
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey || e.key.toLowerCase() !== 'z') return;
      // 焦点在编辑器/输入框：放行（CodeMirror keymap 或浏览器原生撤销）
      const el = document.activeElement;
      if (el instanceof HTMLElement && el.closest('.cm-editor, input, textarea, select, [contenteditable="true"]')) return;
      const view = activeEditorView;
      if (!view) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.shiftKey) redo(view); else undo(view);
    };
    document.addEventListener('keydown', handler, true);
    return () => document.removeEventListener('keydown', handler, true);
  }, []);

  // 定位请求（搜索结果点击）：滚动到指定行 + 高亮全文命中词
  useEffect(() => {
    if (!locate || locate.path !== filePath) return;
    const view = viewRef.current;
    if (!view) return;
    const doc = view.state.doc;
    const lineNo = Math.max(1, Math.min(locate.line, doc.lines));
    const lineFrom = doc.line(lineNo).from;

    // 全文查找命中词位置（上限 500 处防极端；搜索为单行子串匹配，命中必在行内）
    const hits: { from: number; to: number }[] = [];
    if (locate.query) {
      const q = locate.query;
      const lowerQ = q.toLowerCase();
      const qChars = Array.from(q).length;
      for (let n = 1; n <= doc.lines && hits.length < 500; n++) {
        const line = doc.line(n);
        const hay = line.text.toLowerCase();
        // 先判有没有命中，再决定是否物化字符数组（PERF-12）：
        // `Array.from` 把一个字符串摊成码点数组是这里最贵的操作，而全文绝大多数行
        // 都不含搜索词。实测 200k 行文档：无条件物化 174ms → 命中后才物化 29ms（3.3×）。
        // `indexOf` 的判空与下面 while 的首次判断完全等价，故不改变语义。
        if (hay.indexOf(lowerQ) === -1) continue;
        // toLowerCase 可能改变个别字符的码点长度（如 İ→i̇），小写副本的偏移
        // 不能直接用于原文本——先映射为码点序号，再换算回原文本偏移
        const lineChars = Array.from(line.text);
        const hayChars = Array.from(hay);
        let pos = 0;
        while (hits.length < 500 && (pos = hay.indexOf(lowerQ, pos)) !== -1) {
          let charIdx = 0;
          for (let i = 0; i < pos; ) i += hayChars[charIdx++].length;
          let fromOff = 0;
          for (let i = 0; i < charIdx; i++) fromOff += lineChars[i].length;
          let toOff = fromOff;
          for (let i = charIdx; i < charIdx + qChars; i++) toOff += lineChars[i].length;
          hits.push({ from: line.from + fromOff, to: line.from + toOff });
          pos += q.length;
        }
      }
    }

    view.dispatch({
      selection: { anchor: lineFrom },
      effects: [
        EditorView.scrollIntoView(lineFrom, { y: 'center' }),
        searchHitEffect.of(hits),
      ],
    });
    clearLocate();
  }, [locate, filePath, clearLocate]);

  // 搜索弹窗关闭：清空命中高亮（首轮渲染的无害空 dispatch 除外）
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    view.dispatch({ effects: searchHitEffect.of([]) });
  }, [hitSeq]);

  return (
    <div className="relative h-full">
      <div ref={editorRef} className="h-full" />
      <MatchMarksOverlay matchMarks={matchMarks} />
    </div>
  );
}
