import { useRef, useState, useEffect } from 'react';
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, Decoration, type Panel } from '@codemirror/view';
import { EditorState, StateEffect, StateField } from '@codemirror/state';
import { defaultKeymap, history, historyKeymap, redo, undo, toggleBlockCommentByLine, toggleComment } from '@codemirror/commands';
import { syntaxHighlighting, defaultHighlightStyle, indentOnInput, bracketMatching, foldGutter, foldKeymap, syntaxTree, type Language, type LanguageSupport } from '@codemirror/language';
import { search, openSearchPanel, findNext, findPrevious, closeSearchPanel, setSearchQuery, SearchQuery } from '@codemirror/search';
import { createRoot } from 'react-dom/client';
import { useEditorStore, saveActiveFile } from '../../stores/editor';
import { indentationMarkers } from '@replit/codemirror-indentation-markers';
import { oneDark } from '@codemirror/theme-one-dark';
import { javascript } from '@codemirror/lang-javascript';
import { python } from '@codemirror/lang-python';
import { java } from '@codemirror/lang-java';
import { css } from '@codemirror/lang-css';
import { html } from '@codemirror/lang-html';
import { json } from '@codemirror/lang-json';
import { markdown } from '@codemirror/lang-markdown';
import { rust } from '@codemirror/lang-rust';
import { go } from '@codemirror/lang-go';
import { sql } from '@codemirror/lang-sql';
import { vue } from '@codemirror/lang-vue';
import { xml } from '@codemirror/lang-xml';

interface CodeViewerProps {
  filePath: string;
  content: string;
  /** 是否可编辑（默认 true） */
  editable?: boolean;
  /** 内容变更回调（每次编辑触发，父组件写回 store） */
  onChange?: (content: string) => void;
}

/**
 * 已打开文件的编辑器状态缓存（模块级，跨组件实例存活）：
 * 切换标签页时编辑器销毁重建，重建 EditorState 会清空 history（Ctrl+Z 失效）——
 * 缓存 state 复用可保留撤销历史与光标位置。内容被外部修改（watcher 刷新等）
 * 导致与缓存不一致时自动失效重建。上限防止无限增长。
 */
const MAX_CACHED_STATES = 30;
const stateCache = new Map<string, EditorState>();

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

  useEffect(() => { inputRef.current?.focus(); }, []);

  // 应用搜索：更新 query → 统计匹配与当前序号 → 跳转第一个
  const applySearch = (search: string, cs: boolean, re: boolean, ww: boolean) => {
    let q: SearchQuery | null = null;
    try {
      q = new SearchQuery({ search, caseSensitive: cs, regexp: re, wholeWord: ww });
      setInvalid(false);
    } catch {
      setInvalid(true); // 正则无效
      setTotal(0);
      setCurrent(0);
      return;
    }
    view.dispatch({ effects: setSearchQuery.of(q) });
    // 统计匹配数，并定位光标位置后的第一个匹配序号
    // （getCursor 内部处理大小写/正则/全词选项）
    let t = 0;
    let cur = 0;
    const head = view.state.selection.main.head;
    const cursor = q.getCursor(view.state.doc);
    let match = cursor.next();
    while (!match.done) {
      t++;
      if (cur === 0 && match.value.from >= head) cur = t;
      match = cursor.next();
    }
    setTotal(t);
    setCurrent(cur === 0 ? t : cur);
    if (search) findNext(view);
  };

  const handleInput = (v: string) => {
    setValue(v);
    applySearch(v, caseSensitive, regexp, wholeWord);
  };
  const toggleCase = () => { const v = !caseSensitive; setCaseSensitive(v); applySearch(value, v, regexp, wholeWord); };
  const toggleRegexp = () => { const v = !regexp; setRegexp(v); applySearch(value, caseSensitive, v, wholeWord); };
  const toggleWholeWord = () => { const v = !wholeWord; setWholeWord(v); applySearch(value, caseSensitive, regexp, v); };
  const goPrev = () => { if (value) findPrevious(view); };
  const goNext = () => { if (value) findNext(view); };

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
          if (e.key === 'Enter') { if (e.shiftKey) goPrev(); else goNext(); }
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

/** 根据文件扩展名获取语言支持 */
function getLanguageExtension(filePath: string) {
  const ext = filePath.split('.').pop()?.toLowerCase() ?? '';

  const langMap: Record<string, () => Language | LanguageSupport> = {
    js: () => javascript(),
    jsx: () => javascript({ jsx: true }),
    ts: () => javascript({ typescript: true }),
    tsx: () => javascript({ jsx: true, typescript: true }),
    mjs: () => javascript(),
    cjs: () => javascript(),
    py: () => python(),
    java: () => java(),
    css: () => css(),
    scss: () => css(),
    less: () => css(),
    html: () => html(),
    htm: () => html(),
    json: () => json(),
    md: () => markdown(),
    rs: () => rust(),
    go: () => go(),
    sql: () => sql(),
    vue: () => vue(),
    xml: () => xml(),
    class: () => java(), // .class 显示反编译后的 Java 源码，用 Java 高亮
  };

  const factory = langMap[ext];
  return factory ? [factory()] : [];
}

/** 创建 CodeMirror 编辑器状态（含语言支持和主题） */
function createEditorState(
  content: string,
  filePath: string,
  editable: boolean,
  onChange: (content: string) => void,
) {
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
      // 仅编辑器聚焦时触发（keymap 只接收编辑器的键盘事件）
      { key: 'Mod-f', run: openSearchPanel, preventDefault: true },
      { key: 'F3', run: findNext, shift: findPrevious, preventDefault: true },
      { key: 'Mod-g', run: findNext, shift: findPrevious, preventDefault: true },
      // Vue 模板注释修复（lang-vue 缺陷）：需先于语言包 keymap 注册才优先匹配；
      // template 区域 HTML 注释，script/style 区域返回 false 交由语言包处理
      ...(filePath.toLowerCase().endsWith('.vue') ? [{ key: 'Mod-/', run: vueCommentToggle }] : []),
    ]),
    ...getLanguageExtension(filePath),
    EditorView.theme({
      '&': {
        height: '100%',
        // 缩进对齐线颜色：普通线比背景（#282c34）略亮，活动块线用 accent 色系
        '--indent-marker-bg-color': '#3a3f4b',
        '--indent-marker-active-bg-color': '#4a5a7a',
      },
      '.cm-scroller': { overflow: 'auto' },
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
      // 搜索结果命中高亮（背景色）
      '.cm-search-hit': {
        backgroundColor: 'rgba(240, 190, 60, 0.22)',
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
    }),
  ];

  if (editable) {
    extensions.push(
      // 变更监听：写回 store（配合父组件的 updateDraft 标记未保存）
      EditorView.updateListener.of(update => {
        if (update.docChanged) onChange(update.state.doc.toString());
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

export function CodeViewer({ filePath, content, editable = true, onChange }: CodeViewerProps) {
  const editorRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  // 回调与最新内容存 ref：编辑器只在 filePath 变化时重建，
  // 编辑中 content 回写（onChange → store）不触发重建，避免光标/焦点丢失
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const contentRef = useRef(content);
  contentRef.current = content;

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
      // 记录会话起点（= 撤销历史锚点）：Ctrl+Z 撤销回该内容时不算未保存
      useEditorStore.getState().markSessionStart(currentContent);
      state = createEditorState(
        currentContent,
        filePath,
        editable,
        (doc) => {
          onChangeRef.current?.(doc);
          // EditorState 不可变：每次编辑产生新 state 对象，缓存里的引用会过期
          // （doc 对比失败 → 切换回来重建 → 撤销历史丢失）。编辑后把最新
          // state 写回缓存，切换回来 doc 对比命中、Ctrl+Z 历史保留
          if (viewRef.current) stateCache.set(key, viewRef.current.state);
        },
      );
      stateCache.set(key, state);
      if (stateCache.size > MAX_CACHED_STATES) {
        // 超出上限：淘汰最早缓存（Map 迭代顺序 = 插入顺序）
        const oldestKey = stateCache.keys().next().value;
        if (oldestKey !== undefined) stateCache.delete(oldestKey);
      }
    }

    viewRef.current = new EditorView({
      state,
      parent: editorRef.current,
    });
    activeEditorView = viewRef.current;

    return () => {
      if (activeEditorView === viewRef.current) activeEditorView = null;
      viewRef.current?.destroy();
    };
  }, [filePath, editable, openSeq]);

  // 光标不在编辑器时 Ctrl+F 完全无效：
  // 焦点若不在 .cm-editor 内（点击空白/其他面板后编辑器失焦），在捕获阶段吞掉事件，
  // 避免触发文件内搜索面板或任何自带查找行为；编辑器聚焦时放行（keymap 正常处理）
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 'f') {
        const el = document.activeElement;
        if (el && !(el instanceof Element && el.closest('.cm-editor'))) {
          e.preventDefault();
          e.stopPropagation();
        }
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

  return <div ref={editorRef} className="h-full" />;
}
