import { useState } from 'react';
import type { OpenCodeError, OpenCodeSession } from '../../stores/opencodeStore';

/**
 * OpenCode 助手面板：iframe 内嵌官方 Web UI（opencode serve 自带前端，见后端 opencode.rs）。
 * 状态分层：installing（下载二进制）→ starting（服务就绪中）→ 就绪 iframe；
 * 错误分流：缺二进制/下载失败 → 「下载并启动」，启动失败 → 「重试」。
 */
export function OpenCodePanel({ session, starting, installing, error, onRetry, onInstall, onUpdate, onHide, onClose }: {
  session: OpenCodeSession;
  starting: boolean;
  installing: boolean;
  error: OpenCodeError | null;
  onRetry: () => void;
  onInstall: () => void;
  onUpdate: () => void;
  onHide: () => void;
  onClose: () => void;
}) {
  // 刷新：改 key 强制重建 iframe
  const [reloadKey, setReloadKey] = useState(0);
  const busy = installing || starting;
  const ready = !busy && !error && session.port !== null;
  // 缺二进制 / 下载失败 → 主操作是「下载并启动」；纯启动失败 → 「重试」
  const needInstall = !!error && (error.kind === 'missing' || error.kind === 'install');

  return (
    // w-full：外层 wrapper 固定 600px，此节点作为 flex 子项默认按内容收缩，
    // 不加 w-full 会导致 iframe 撑不满、右侧残留一列空位
    <div className="flex flex-col h-full w-full min-w-0 bg-nexus-surface border-l border-nexus-border">
      {/* 头部 */}
      <div className="flex items-center gap-2 px-3 h-[42px] border-b border-nexus-border flex-shrink-0">
        <svg width="13" height="13" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3" className="text-nexus-accent flex-shrink-0">
          <path d="M7 1.6l1.7 3.7 3.7 1.7-3.7 1.7L7 12.4 5.3 8.7 1.6 7l3.7-1.7z"/>
        </svg>
        <span className="text-[13px] text-nexus-text font-medium truncate min-w-0">{session.name}</span>
        {ready && (
          <span className="flex items-center gap-1 text-[11px] text-nexus-muted flex-shrink-0">
            <span className="w-[6px] h-[6px] rounded-full bg-nexus-success inline-block" />
            :{session.port}
          </span>
        )}
        <div className="flex-1" />
        {/* 更新：仅自管二进制会话可用（stop → 下载最新 → 同项目重启） */}
        {ready && session.managed && (
          <button
            className="p-1.5 text-nexus-muted hover:text-nexus-accent rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
            title="检查更新（重新下载最新版并重启当前会话）"
            onClick={onUpdate}
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 5v14M19 12l-7 7-7-7"/>
            </svg>
          </button>
        )}
        <button
          className="p-1.5 text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
          title="重新加载界面"
          onClick={() => setReloadKey(k => k + 1)}
        >
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/>
          </svg>
        </button>
        <button
          className="p-1.5 text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
          title="收起（保留会话，点击左侧按钮可再展开）"
          onClick={onHide}
        >
          <svg width="12" height="12" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
            <polyline points="9,2 4,7 9,12"/>
          </svg>
        </button>
        <button
          className="p-1.5 text-nexus-muted hover:text-nexus-error rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
          title="关闭（终止 opencode 服务）"
          onClick={onClose}
        >
          <svg width="12" height="12" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
            <path d="M3 3l8 8M11 3l-8 8"/>
          </svg>
        </button>
      </div>

      {/* 内容区 */}
      {installing ? (
        <div className="flex-1 flex flex-col items-center justify-center gap-2 px-6 text-center">
          <div className="w-4 h-4 border-2 border-nexus-accent/30 border-t-nexus-accent rounded-full animate-spin" />
          <span className="text-[12px] text-nexus-muted">正在下载并安装 opencode 最新版…</span>
          <span className="text-[11px] text-nexus-muted/70">首次下载约 30–60 MB，完成后自动启动</span>
        </div>
      ) : error ? (
        <div className="flex-1 flex flex-col items-center justify-center gap-2 px-6 py-6 min-h-0">
          <span className="text-[12px] text-nexus-error text-center break-all max-h-40 overflow-auto leading-relaxed">{error.message}</span>
          {needInstall && (
            <span className="text-[11px] text-nexus-muted/80 text-center leading-relaxed">
              {error.kind === 'missing'
                ? '点击「下载并启动」自动安装最新版（约 30–60 MB），完成后直接打开，无需预装任何环境'
                : '下载失败可稍后重试（多为网络或上游发布临时变动导致）'}
            </span>
          )}
          <div className="flex gap-2 mt-1">
            {needInstall ? (
              <button
                className="px-3 py-1 text-[12px] bg-nexus-accent text-white rounded-md hover:bg-nexus-accent-hover"
                onClick={onInstall}
              >下载并启动</button>
            ) : (
              <button
                className="px-3 py-1 text-[12px] bg-nexus-accent text-white rounded-md hover:bg-nexus-accent-hover"
                onClick={onRetry}
              >重试</button>
            )}
            <button
              className="px-3 py-1 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50"
              onClick={onClose}
            >关闭</button>
          </div>
        </div>
      ) : starting ? (
        <div className="flex-1 flex flex-col items-center justify-center gap-2 px-6 text-center">
          <div className="w-4 h-4 border-2 border-nexus-accent/30 border-t-nexus-accent rounded-full animate-spin" />
          <span className="text-[12px] text-nexus-muted">正在启动 OpenCode…</span>
          <span className="text-[11px] text-nexus-muted/70">首次经 npx 启动需联网下载，最多约 30 秒</span>
        </div>
      ) : (
        <iframe
          key={`${session.cwd}-${session.port}-${reloadKey}`}
          className="flex-1 w-full border-0 min-h-0"
          src={`http://127.0.0.1:${session.port}`}
          title={`OpenCode — ${session.name}`}
        />
      )}
    </div>
  );
}
