import { useRef, useEffect } from 'react';
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter } from '@codemirror/view';
import { EditorState } from '@codemirror/state';
import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
import { syntaxHighlighting, defaultHighlightStyle, indentOnInput, bracketMatching, foldGutter, foldKeymap } from '@codemirror/language';
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

interface CodeViewerProps {
  filePath: string;
  content: string;
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
  };

  const factory = langMap[ext];
  return factory ? [factory()] : [];
}

/** 创建 CodeMirror 编辑器状态（含语言支持和主题） */
function createEditorState(content: string, filePath: string) {
  return EditorState.create({
    doc: content,
    extensions: [
      lineNumbers(),
      highlightActiveLine(),
      highlightActiveLineGutter(),
      indentOnInput(),
      bracketMatching(),
      foldGutter(),
      history(),
      syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
      oneDark,
      keymap.of([...defaultKeymap, ...historyKeymap, ...foldKeymap]),
      EditorView.editable.of(false),
      ...getLanguageExtension(filePath),
      EditorView.theme({
        '&': { height: '100%' },
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
      }),
    ],
  });
}

export function CodeViewer({ filePath, content }: CodeViewerProps) {
  const editorRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);

  useEffect(() => {
    if (!editorRef.current) return;

    viewRef.current?.destroy();
    const state = createEditorState(content, filePath);

    viewRef.current = new EditorView({
      state,
      parent: editorRef.current,
    });

    return () => {
      viewRef.current?.destroy();
    };
  }, [content, filePath]);

  return <div ref={editorRef} className="h-full" />;
}
