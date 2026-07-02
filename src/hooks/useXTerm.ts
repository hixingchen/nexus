import { useRef, useCallback } from 'react';
import { Terminal as XTerm, type ITerminalOptions } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { SearchAddon } from '@xterm/addon-search';
import '@xterm/xterm/css/xterm.css';

/** 根据终端配置创建 xterm 初始化参数 */
function createXTermOptions(): ITerminalOptions {
  return {
    fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', Consolas, 'Courier New', monospace",
    fontSize: 14,
    cursorStyle: 'block',
    cursorBlink: true,
    scrollback: 10000,
    lineHeight: 1.2,
    theme: {
      background: '#282c34',
      foreground: '#abb2bf',
      cursor: '#528bff',
      cursorAccent: '#282c34',
      selectionBackground: '#3e4451',
      selectionForeground: '#abb2bf',
      black: '#282c34',
      red: '#e06c75',
      green: '#98c379',
      yellow: '#d19a66',
      blue: '#61afef',
      magenta: '#c678dd',
      cyan: '#56b6c2',
      white: '#abb2bf',
      brightBlack: '#5c6370',
      brightRed: '#e06c75',
      brightGreen: '#98c379',
      brightYellow: '#d19a66',
      brightBlue: '#61afef',
      brightMagenta: '#c678dd',
      brightCyan: '#56b6c2',
      brightWhite: '#ffffff',
    },
    tabStopWidth: 4,
    allowProposedApi: true,
    convertEol: true,
    scrollOnUserInput: true,
  };
}

/**
 * 管理 xterm.js 实例的生命周期：创建、配置、销毁
 */
export function useXTerm(containerRef: React.RefObject<HTMLDivElement | null>, sessionId: string) {
  const xtermRef = useRef<XTerm | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const searchAddonRef = useRef<SearchAddon | null>(null);

  const fitTerminal = useCallback(() => {
    if (fitAddonRef.current && xtermRef.current) {
      try {
        fitAddonRef.current.fit();
      } catch {
        // ignore fit errors (container not visible, etc.)
      }
    }
  }, []);

  // 初始化 xterm 实例
  const initXTerm = useCallback(() => {
    if (!containerRef.current) return null;

    const xterm = new XTerm(createXTermOptions());

    const fitAddon = new FitAddon();
    const webLinksAddon = new WebLinksAddon();
    const searchAddon = new SearchAddon();

    xterm.loadAddon(fitAddon);
    xterm.loadAddon(webLinksAddon);
    xterm.loadAddon(searchAddon);

    xterm.open(containerRef.current);
    // 延迟 fit 确保 DOM 已完全渲染
    setTimeout(() => {
      try {
        fitAddon.fit();
      } catch {
        // ignore fit errors
      }
    }, 0);

    xtermRef.current = xterm;
    fitAddonRef.current = fitAddon;
    searchAddonRef.current = searchAddon;

    return xterm;
  }, [containerRef]);

  // 搜索功能
  const search = useCallback((text: string, options?: { regex?: boolean; wholeWord?: boolean; caseSensitive?: boolean }) => {
    searchAddonRef.current?.findNext(text, options);
  }, []);

  const searchPrevious = useCallback((text: string, options?: { regex?: boolean; wholeWord?: boolean; caseSensitive?: boolean }) => {
    searchAddonRef.current?.findPrevious(text, options);
  }, []);

  const clearSearch = useCallback(() => {
    searchAddonRef.current?.clearActiveDecoration();
  }, []);

  // 复制/粘贴/清屏
  const copySelection = useCallback(() => {
    const selection = xtermRef.current?.getSelection();
    if (selection) navigator.clipboard.writeText(selection);
  }, []);

  const paste = useCallback(async () => {
    try {
      const text = await navigator.clipboard.readText();
      if (text && xtermRef.current) {
        const { writeTerminal } = await import('../services/terminal');
        await writeTerminal(sessionId, text);
      }
    } catch (err) {
      console.error('粘贴失败:', err);
    }
  }, [sessionId]);

  const clear = useCallback(() => {
    xtermRef.current?.clear();
  }, []);

  // 清理
  const dispose = useCallback(() => {
    xtermRef.current?.dispose();
    xtermRef.current = null;
    fitAddonRef.current = null;
    searchAddonRef.current = null;
  }, []);

  return {
    xtermRef,
    fitAddonRef,
    initXTerm,
    fitTerminal,
    search,
    searchPrevious,
    clearSearch,
    copySelection,
    paste,
    clear,
    dispose,
  };
}
