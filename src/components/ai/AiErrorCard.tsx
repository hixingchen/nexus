import { useState } from 'react';
import type { AiError, AiRuntimeStatus } from '../../services/aiService';
import { copyText } from '../../utils/clipboard';
import { RobotIcon } from './RobotIcon';

/**
 * AI 面板的错误卡：**先说卡在哪、再说人话、最后贴原话**。
 *
 * 为什么不是弹窗：弹窗会被条件反射地关掉、关掉就没了、也没法复制——而失败恰恰需要用户
 * 停下来看清楚。所以失败一律走"就地卡片"：常驻、可回看、可复制。弹窗只留给
 * **用户马上要做一件有代价的事、且还有回头路**的场景（确认卸载），见设置里的运行时小节。
 *
 * 三样信息对应的正是本轮事故里缺的东西：
 * - 原来只会说"未检测到 dsh，请先安装"（假话：dsh 装着，是 Node 跑不动）
 * - 点安装又失败，且**原因没留下**（`npm` 的原话被丢掉了）
 * - 用户不知道该做什么（重试多少次都一样，因为原因不在重试上）
 */
export function AiErrorCard({
  error,
  status,
  busy,
  onInstallRuntime,
  onReinstallRuntime,
  onClearManual,
  onRetry,
}: {
  error: AiError;
  /** 运行时状态（拼"复制诊断信息"用；拿不到就不带那一段） */
  status: AiRuntimeStatus | null;
  busy: boolean;
  onInstallRuntime: () => void;
  onReinstallRuntime: () => void;
  /** 当前跑的是**手动指定**的那份时，给一条明确的退路（见下） */
  onClearManual: () => void;
  onRetry: () => void;
}) {
  const [showRaw, setShowRaw] = useState(false);
  const runtimeMissing = error.kind === 'runtime_missing';
  // 装了但跑不起来：主操作是"清干净重来"。但**手动指定的那份不能重装**——
  // 那不是 Nexus 装的，删了就是删用户的文件；这时唯一的出路是"清除手动指定、回到自带那份"。
  // 手动路径优先且不静默回落（这是有意的），所以这条退路必须显式摆在卡片上。
  const manual = status?.source === 'manual';
  const runtimeBroken = error.kind === 'runtime_broken';

  const title = runtimeMissing
    ? 'AI 运行时还没准备'
    : runtimeBroken
      ? 'AI 运行时用不了'
      : 'AI 会话启动失败';

  return (
    <div className="h-full flex flex-col items-center justify-center gap-2 px-8 text-center overflow-auto">
      <span className="text-nexus-accent/70"><RobotIcon size={28} /></span>
      <p className="text-[13px] text-nexus-text font-medium">{title}</p>
      {error.step && (
        <p className="text-[11px] text-nexus-muted">
          卡在：<span className="text-nexus-text">{error.step}</span>
        </p>
      )}
      <p className="text-[11.5px] text-nexus-muted leading-relaxed break-all max-w-[460px]">{error.summary}</p>

      {error.raw && (
        <div className="w-full max-w-[460px]">
          <button
            className="text-[11px] text-nexus-muted hover:text-nexus-text underline underline-offset-2"
            onClick={() => setShowRaw((v) => !v)}
          >
            {showRaw ? '收起原始输出' : '查看原始输出'}
          </button>
          {showRaw && (
            <pre className="mt-1.5 max-h-[26vh] overflow-auto text-left text-[11px] leading-relaxed text-nexus-muted bg-nexus-editor border border-nexus-border rounded-md p-2.5 whitespace-pre-wrap break-all">
              {error.raw}
            </pre>
          )}
        </div>
      )}

      <div className="mt-1 flex items-center gap-2 flex-wrap justify-center">
        {runtimeMissing && (
          <PrimaryButton onClick={onInstallRuntime} disabled={busy}>
            {busy ? '正在准备…' : '准备 AI 运行时（下载约 300MB）'}
          </PrimaryButton>
        )}
        {runtimeBroken && !manual && (
          <PrimaryButton onClick={onReinstallRuntime} disabled={busy}>
            {busy ? '正在重装…' : '重新安装运行时'}
          </PrimaryButton>
        )}
        {runtimeBroken && manual && (
          <PrimaryButton onClick={onClearManual} disabled={busy}>
            {busy ? '正在切换…' : '清除手动指定，回到自带那份'}
          </PrimaryButton>
        )}
        {!busy && <GhostButton onClick={onRetry}>重试</GhostButton>}
        <GhostButton onClick={() => void copyText(diagnostics(error, status), '诊断信息')}>
          复制诊断信息
        </GhostButton>
      </div>

      {runtimeMissing && (
        <p className="text-[11px] text-nexus-muted/80 leading-relaxed max-w-[440px]">
          只需下载一次。之后 AI 用 Nexus 自带的那份 Node，
          <span className="text-nexus-text">与你机器上的 nvm / Node 版本完全无关</span>。
        </p>
      )}
    </div>
  );
}

/**
 * 拼一段可直接粘贴给别人的诊断文本。
 *
 * 为什么值得单独做：Nexus 会被发给同事用，同事出问题时"复制这个 → 发给我"比
 * "你截图给我看看"精确得多——里面有版本、路径和原话，一眼能定位到哪一步断的。
 */
export function diagnostics(error: AiError, status: AiRuntimeStatus | null): string {
  const lines: string[] = ['[Nexus AI 诊断]'];
  if (status) {
    lines.push(`运行时：${status.ready ? '已就绪' : '未准备'}`);
    lines.push(`自带 Node：${status.node_version ?? '—'}`);
    lines.push(`dsh：${status.dsh_version ?? '—'}`);
    lines.push(`安装目录：${status.path}`);
  }
  lines.push(`错误类型：${error.kind}`);
  if (error.step) lines.push(`卡在：${error.step}`);
  lines.push(`摘要：${error.summary}`);
  if (error.raw) lines.push('原始输出：', error.raw);
  return lines.join('\n');
}

function PrimaryButton({
  onClick, disabled, children,
}: {
  onClick: () => void; disabled?: boolean; children: React.ReactNode;
}) {
  return (
    <button
      className="px-3 py-1 text-[12px] bg-nexus-accent/15 text-nexus-accent rounded-md hover:bg-nexus-accent/25 disabled:opacity-40 transition-colors"
      disabled={disabled}
      onClick={onClick}
    >{children}</button>
  );
}

function GhostButton({ onClick, children }: { onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      className="px-3 py-1 text-[12px] text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 transition-colors"
      onClick={onClick}
    >{children}</button>
  );
}
