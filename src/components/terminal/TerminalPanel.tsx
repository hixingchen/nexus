import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { ptyApi } from '../../services/service';
import { showNotification } from '../ui/Toast';

interface Props {
  /** 工作目录（服务路径） */
  cwd: string;
  /** 面板标题（服务名） */
  serviceName: string;
  /** 关闭面板（同时杀死 claude 会话） */
  onClose: () => void;
}

/**
 * 内嵌终端面板（xterm.js + ConPTY）
 *
 * xterm.js 渲染在 WebView 内，PTY 由 Rust 后端创建并连接 claude CLI。
 * IME 说明：xterm v5+ 自绘组合文本，拼音候选期间不会把中间态写入 PTY，
 * 仅在 compositionend 后整体经 onData 送出——中文输入正常可用。
 *
 * 尺寸同步：ResizeObserver 监听容器（窗口缩放/列拖宽/收起→展开全覆盖），
 * fit 后经 terminal.onResize 自动同步 PTY 行列数。
 *
 * 重启策略：xterm 实例常驻，用「会话代次」作废旧会话事件——
 * kill+spawn 后旧读线程的 eof 事件不会污染新会话状态。
 */
export function TerminalPanel({ cwd, serviceName, onClose }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  /** 会话代次：每次 spawn 递增，eof/data 事件仅接受当前代 */
  const epochRef = useRef(0);
  /** 最近一次重启/关闭的时间戳：旧会话被 kill 后的 eof 会在毫秒级到达，需忽略 */
  const killAtRef = useRef(0);
  const [phase, setPhase] = useState<'spawning' | 'up' | 'exited'>('spawning');
  const cwdRef = useRef(cwd);
  cwdRef.current = cwd;

  // 初始化 xterm（常驻）+ pty-output 事件监听（单次绑定）
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const terminal = new Terminal({
      cursorBlink: true,
      fontSize: 13,
      fontFamily: 'Consolas, "Courier New", monospace',
      theme: {
        background: '#1a1a1a',
        foreground: '#d4d4d4',
        cursor: '#d4d4d4',
        selectionBackground: '#264f78',
      },
      allowProposedApi: true,
      scrollback: 5000,
    });
    const fitAddon = new FitAddon();
    fitAddonRef.current = fitAddon;
    terminal.loadAddon(fitAddon);
    terminal.open(host);
    terminalRef.current = terminal;
    // 聚焦输入 textarea：xterm 打开后不会自动聚焦，点击 canvas 也不保证——
    // 无焦点时键盘/粘贴事件没有目标（"Ctrl+V 没反应"的常见根因）
    terminal.focus();
    const onHostClick = () => terminalRef.current?.focus();
    host.addEventListener('mousedown', onHostClick);

    // 输入输出与尺寸事件在初次 fit 之前绑定——否则初次 fit 算出的 cols/rows
    // 不会推送（onResize 只在尺寸变化时触发，之后 fit 无变化就不 fire → PTY
    // 停留在初始 30×120，与渲染列数不一致 → claude TUI 折行错乱，拖一下才对齐）
    let unlisten: (() => void) | undefined;
    let disposed = false;

    // 用户输入 → PTY（IME 组合结束后完整文本经此送出；无自定义键盘拦截，
    // 保证输入法 composition 事件不被破坏）
    const dataDispose = terminal.onData((data) => {
      if (disposed) return;
      ptyApi.write(data).catch((err) => {
        console.error('PTY 写入失败:', err);
      });
    });
    // fit 结果 → PTY resize
    const resizeDispose = terminal.onResize(({ cols, rows }) => {
      ptyApi.resize(rows, cols).catch(() => {});
    });

    // 容器尺寸变化 → fit（隐藏/0 尺寸时跳过）→ 经 onResize 同步 PTY。
    // IME 组合期间禁止 fit：候选框出现常伴随布局抖动，若此刻重算 cols 并 resize，
    // 会迫使 claude 全屏 TUI 按新列宽整体重排（表现为"界面左移/样式变了"）
    const composingRef = { current: false };
    const onComposition = (e: Event) => {
      composingRef.current = e.type === 'compositionstart';
      // 组合结束：布局恢复稳定后补一次 fit（若期间确实被拖宽过）
      if (e.type === 'compositionend') requestAnimationFrame(fit);
    };
    host.addEventListener('compositionstart', onComposition);
    host.addEventListener('compositionend', onComposition);
    const fit = () => {
      if (composingRef.current) return;
      if (host.clientWidth === 0 || host.clientHeight === 0) return;
      try { fitAddon.fit(); } catch { /* 布局未就绪时忽略 */ }
    };
    const ro = new ResizeObserver(fit);
    ro.observe(host);
    fit();

    // 复制/粘贴（Windows Terminal / VS Code 惯例），剪贴板一律走 Rust 后端
    // （tauri-plugin-clipboard-manager）：WebView 的 Clipboard API 在 WebView2 中
    // 存在权限/行为不确定性（实测粘贴无效），后端读写系统剪贴板无此限制。
    // - 有选区：Ctrl+C / Ctrl+Shift+C 复制；无选区：Ctrl+C 照常发给 claude（中断）
    // - Ctrl+V / Ctrl+Shift+V / Shift+Insert：后端读剪贴板 → terminal.paste → PTY
    terminal.attachCustomKeyEventHandler((e) => {
      // xterm 把同一 handler 挂在 keydown/keypress/keyup 上：只处理 keydown，
      // 否则 Ctrl+V 会在 keyup 时（Ctrl 仍按下）重复触发 → 粘贴两次
      if (e.type !== 'keydown') return true;
      // IME 组合中的键（keyCode 229）一律放行，交给输入法处理
      if (e.isComposing || e.keyCode === 229) return true;
      const key = e.key.toLowerCase();
      const ctrl = e.ctrlKey && !e.altKey && !e.metaKey;

      // 复制（有选区才拦截；无选区 Ctrl+C 放行给 claude = 中断）
      const copySelection = () => {
        const text = terminal.getSelection();
        terminal.clearSelection();
        invoke('plugin:clipboard-manager|write_text', { text })
          .catch((err) => console.error('复制到剪贴板失败:', err));
      };
      if (key === 'c' && terminal.hasSelection() &&
          ((ctrl && !e.shiftKey) || (ctrl && e.shiftKey))) {
        copySelection();
        return false;
      }

      // 粘贴：后端读剪贴板（失败给出可见错误，不再静默）
      if (key === 'v' && ctrl) {
        e.preventDefault();
        invoke<string>('plugin:clipboard-manager|read_text')
          .then((t) => {
            if (t) terminal.paste(t);
          })
          .catch((err) => {
            console.error('读取剪贴板失败:', err);
            showNotification({ variant: 'error', title: '粘贴失败', description: String(err) });
          });
        return false;
      }
      // Shift+Insert 粘贴（Windows 惯例）
      if (e.key === 'Insert' && e.shiftKey && !ctrl) {
        e.preventDefault();
        invoke<string>('plugin:clipboard-manager|read_text')
          .then((t) => { if (t) terminal.paste(t); })
          .catch((err) => {
            console.error('读取剪贴板失败:', err);
            showNotification({ variant: 'error', title: '粘贴失败', description: String(err) });
          });
        return false;
      }
      return true;
    });

    // PTY 输出事件（eof/data 都校验代次：重启产生的旧 eof 不落屏不上态）
    const onPtyOutput = (event: { payload: { data?: string; eof?: boolean } }) => {
      if (disposed) return;
      if (event.payload.eof) {
        // 从未启动过会话、或距重启/关闭不足 1s（旧会话被 kill 的残余 eof）→ 忽略
        if (epochRef.current === 0) return;
        if (Date.now() - killAtRef.current < 1000) return;
        setPhase('exited');
        terminal.write('\r\n\x1b[90m[会话已结束]\x1b[0m\r\n');
      } else if (event.payload.data) {
        terminal.write(event.payload.data);
      }
    };

    const beginSession = async (epoch: number) => {
      try {
        // 事件通道先就位（claude 启动初期的输出不丢失；重启时不重复绑定）
        if (!unlisten) {
          const off = await listen<{ data?: string; eof?: boolean }>('pty-output', onPtyOutput);
          if (disposed) return;
          unlisten = off;
        }
        await ptyApi.spawn(cwdRef.current);
        if (disposed || epoch !== epochRef.current) return;
        // 无条件显式推送当前容器尺寸（不依赖 onResize——尺寸与初始相同时它不触发）：
        // PTY 建立后若不推送会停留在初始 30×120，与渲染列数不一致 → claude TUI 折行错乱
        const dims = fitAddon.proposeDimensions();
        if (dims) {
          ptyApi.resize(dims.rows, dims.cols).catch(() => {});
        }
        setPhase('up');
      } catch (err) {
        if (disposed || epoch !== epochRef.current) return;
        setPhase('exited');
        terminal.write(`\x1b[31m启动失败: ${String(err)}\x1b[0m\r\n`);
      }
    };

    // 首会话
    epochRef.current = 1;
    beginSession(1);

    return () => {
      disposed = true;
      ro.disconnect();
      host.removeEventListener('compositionstart', onComposition);
      host.removeEventListener('compositionend', onComposition);
      host.removeEventListener('mousedown', onHostClick);
      unlisten?.();
      dataDispose.dispose();
      resizeDispose.dispose();
      terminal.dispose();
      terminalRef.current = null;
      fitAddonRef.current = null;
      // 组件卸载（切项目/关面板）时杀掉会话，避免后端 claude 进程泄漏
      ptyApi.kill().catch(() => {});
    };
  }, [cwd]);

  // 重启：kill 旧会话 → 清屏 → 新代次 spawn
  const handleRestart = useCallback(async () => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    const epoch = ++epochRef.current;
    killAtRef.current = Date.now();
    setPhase('spawning');
    terminal.clear();
    try {
      await ptyApi.kill().catch(() => {});
      await ptyApi.spawn(cwdRef.current);
      if (epoch === epochRef.current) {
        // 重启后无条件显式推送尺寸（会话重建后 PTY 回到初始 30×120）
        const fitAddon = fitAddonRef.current;
        if (fitAddon) {
          const dims = fitAddon.proposeDimensions();
          if (dims) ptyApi.resize(dims.rows, dims.cols).catch(() => {});
        }
        setPhase('up');
      }
    } catch (err) {
      if (epoch === epochRef.current) {
        setPhase('exited');
        terminal.write(`\x1b[31m重启失败: ${String(err)}\x1b[0m\r\n`);
      }
    }
  }, []);

  const handleClose = useCallback(() => {
    epochRef.current++; // 作废在途事件
    ptyApi.kill().catch(() => {});
    onClose();
  }, [onClose]);

  return (
    <div className="flex flex-col h-full bg-[#1a1a1a]">
      {/* 工具栏 */}
      <div className="flex items-center justify-between px-3 h-[32px] border-b border-nexus-border flex-shrink-0 bg-nexus-surface/90">
        <div className="flex items-center gap-2 min-w-0">
          <svg width="13" height="13" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3" className="text-nexus-muted flex-shrink-0">
            <path d="M2 3.5l5 3.5-5 3.5" /><line x1="8" y1="10" x2="12" y2="10" />
          </svg>
          <span className="text-[11px] text-nexus-text font-medium truncate">{serviceName}</span>
          <span className="text-[10px] text-nexus-muted/60 flex-shrink-0">claude</span>
          {phase === 'spawning' && <span className="text-[10px] text-nexus-muted animate-pulse flex-shrink-0">启动中…</span>}
          {phase === 'exited' && <span className="text-[10px] text-nexus-warning flex-shrink-0">已结束</span>}
        </div>
        <div className="flex items-center gap-0.5 flex-shrink-0">
          <button
            className="p-1 text-nexus-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50 transition-colors"
            title="重启会话"
            onClick={handleRestart}
          >
            <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round">
              <path d="M1.5 6a4.5 4.5 0 018.5-2M10.5 6a4.5 4.5 0 01-8.5 2" />
              <path d="M10 1v3h-3M2 11V8h3" />
            </svg>
          </button>
          <button
            className="p-1 text-nexus-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50 transition-colors"
            title="关闭（结束会话）"
            onClick={handleClose}
          >
            <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
              <path d="M2 2l8 8M10 2l-8 8" />
            </svg>
          </button>
        </div>
      </div>

      {/* 终端内容区（xterm 挂载点） */}
      <div ref={hostRef} className="flex-1 overflow-hidden pl-2" />
    </div>
  );
}
