import { useRef, useEffect } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { Terminal as XTerm } from '@xterm/xterm';
import { useXTerm } from './useXTerm';
import * as terminalService from '../services/terminal';

interface TerminalOutputPayload {
  session_id: string;
  data: string;
}

interface TerminalExitPayload {
  session_id: string;
  exit_code?: number;
}

/**
 * 统一管理终端生命周期：xterm 初始化 → PTY 创建 → 事件监听 → 输入/resize → 清理
 *
 * 单个 effect 保证顺序：先创建 xterm，再创建 PTY，再注册事件。
 * ptySessionId 变化时自动清理旧实例并重建。
 */
export function useTerminalSetup(
  ptySessionId: string,
  containerRef: React.RefObject<HTMLDivElement | null>,
  visible: boolean = true,
  workingDir?: string,
  initCommand?: string,
) {
  const xtermRef = useRef<XTerm | null>(null);
  const createdRef = useRef(false);

  const { initXTerm, fitTerminal } = useXTerm(containerRef, ptySessionId);

  // 面板从隐藏恢复可见时，重新 fit 以正确渲染隐藏期间收到的数据
  useEffect(() => {
    if (visible && xtermRef.current) {
      // 延迟一帧确保 DOM 已完成布局
      requestAnimationFrame(() => fitTerminal());
    }
  }, [visible, fitTerminal]);

  useEffect(() => {
    let disposed = false;
    const unlisteners: UnlistenFn[] = [];
    let cleanupResize: (() => void) | null = null;

    const setup = async () => {
      // 1. 初始化 xterm（同步，立即可用）
      const xterm = initXTerm();
      if (!xterm || disposed) return;
      xtermRef.current = xterm;

      // 2. 注册事件监听（async，但先于 PTY 创建）
      unlisteners.push(
        await listen<TerminalOutputPayload>('terminal-output', (event) => {
          if (!disposed && event.payload.session_id === ptySessionId) {
            xterm.write(event.payload.data);
          }
        }),
        await listen<TerminalExitPayload>('terminal-exit', (event) => {
          if (!disposed && event.payload.session_id === ptySessionId) {
            const msg = event.payload.exit_code !== undefined
              ? `\r\n\x1b[33m[进程已退出，代码: ${event.payload.exit_code}]\x1b[0m\r\n`
              : '\r\n\x1b[33m[进程已退出]\x1b[0m\r\n';
            xterm.write(msg);
          }
        }),
      );

      if (disposed) return;

      // 3. 注册输入处理
      xterm.onData(async (data) => {
        try { await terminalService.writeTerminal(ptySessionId, data); }
        catch (err) { console.error('写入终端失败:', err); }
      });
      xterm.onBinary(async (data) => {
        try { await terminalService.writeTerminal(ptySessionId, data); }
        catch (err) { console.error('写入终端二进制数据失败:', err); }
      });

      // 4. 创建 PTY（事件监听已就绪，不会丢失输出）
      if (!createdRef.current) {
        try {
          await terminalService.createTerminal(ptySessionId, {
            workingDir,
            cols: xterm.cols,
            rows: xterm.rows,
            initCommand,
          });
          createdRef.current = true;
        } catch (err) {
          console.error('创建终端失败:', err);
          xterm.write(`\x1b[31m创建终端失败: ${err}\x1b[0m\r\n`);
          return;
        }
      }

      // 5. 注册 resize 处理
      const handleResize = () => fitTerminal();
      window.addEventListener('resize', handleResize);
      const resizeObserver = new ResizeObserver(() => {
        requestAnimationFrame(() => fitTerminal());
      });
      if (xterm.element?.parentElement) {
        resizeObserver.observe(xterm.element.parentElement);
      }
      cleanupResize = () => {
        window.removeEventListener('resize', handleResize);
        resizeObserver.disconnect();
      };
    };

    setup();

    return () => {
      disposed = true;
      cleanupResize?.();
      unlisteners.forEach(fn => fn());
      xtermRef.current?.dispose();
      xtermRef.current = null;
      // 关闭 PTY 会话
      if (createdRef.current) {
        terminalService.closeTerminal(ptySessionId).catch(() => {});
        createdRef.current = false;
      }
    };
  }, [ptySessionId, initXTerm, fitTerminal]);

  return {
    xtermRef,
    copySelection: () => {
      const selection = xtermRef.current?.getSelection();
      if (selection) navigator.clipboard.writeText(selection);
    },
    paste: async () => {
      try {
        const text = await navigator.clipboard.readText();
        if (text) {
          await terminalService.writeTerminal(ptySessionId, text);
        }
      } catch (err) {
        console.error('粘贴失败:', err);
      }
    },
  };
}
