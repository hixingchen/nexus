import { useRef, useState, useEffect } from 'react';
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, Decoration, type Panel } from '@codemirror/view';
import { EditorState, StateEffect, StateField } from '@codemirror/state';
import { defaultKeymap, history, historyKeymap, redo } from '@codemirror/commands';
import { syntaxHighlighting, defaultHighlightStyle, indentOnInput, bracketMatching, foldGutter, foldKeymap } from '@codemirror/language';
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

interface CodeViewerProps {
  filePath: string;
  content: string;
  /** 是否可编辑（默认 true） */
  editable?: boolean;
  /** 内容变更回调（每次编辑触发，父组件写回 store） */
  onChange?: (content: string) => void;
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

/** 根据文件扩展名获取语言支持 */
function getLanguageExtension(filePath: string) {
  const ext = filePath.split('.').pop()?.toLowerCase() ?? '';

  const langMap: Record<string, () => ReturnType<typeof javascript>> = {
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
    extensions.push(EditorView.editable.of(false));
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

  useEffect(() => {
    if (!editorRef.current) return;

    viewRef.current?.destroy();
    const state = createEditorState(
      contentRef.current,
      filePath,
      editable,
      (doc) => onChangeRef.current?.(doc),
    );

    viewRef.current = new EditorView({
      state,
      parent: editorRef.current,
    });

    return () => {
      viewRef.current?.destroy();
    };
  }, [filePath, editable]);

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
