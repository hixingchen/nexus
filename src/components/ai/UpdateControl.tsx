import { useState } from 'react';
import { Modal } from '../ui/Modal';
import { aiService, type DshVersionInfo } from '../../services/aiService';

/** 弹窗状态机 */
type Phase =
  | { kind: 'checking' }
  | { kind: 'result'; info: DshVersionInfo }
  | { kind: 'error'; message: string };

interface UpdateControlProps {
  /** 确认安装/升级 → 交给父级统一执行（进度显示在 AI 面板主体，见 AiPanel.handleInstallDsh） */
  onInstall: () => void;
}

/**
 * 「检查更新」按钮 + 弹窗（dsh 引擎版本）：
 * 弹窗只负责「联网查询 + 展示 + 用户确认」——点安装/升级即关弹窗，
 * 实际执行在 AI 面板主体统一显示「正在安装…」（与错误卡「安装 dsh」同入口），
 * 避免两处各播各的进度
 */
export function UpdateControl({ onInstall }: UpdateControlProps) {
  const [open, setOpen] = useState(false);
  const [phase, setPhase] = useState<Phase>({ kind: 'checking' });

  /** 打开弹窗并立即检查 */
  const handleOpen = () => {
    setOpen(true);
    void check();
  };

  const check = async () => {
    setPhase({ kind: 'checking' });
    try {
      const info = await aiService.checkUpdate();
      setPhase({ kind: 'result', info });
    } catch (e) {
      setPhase({ kind: 'error', message: String(e) });
    }
  };

  /** 确认安装/升级：关弹窗，执行交给面板主体（避免弹窗锁进度、面板另走一套） */
  const handleUpgrade = () => {
    if (phase.kind !== 'result') return;
    setOpen(false);
    onInstall();
  };

  const installing = phase.kind === 'result' && !phase.info.dshFound;

  return (
    <>
      <button
        className="w-6 h-6 flex items-center justify-center rounded text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/60"
        title="检查更新（dsh 引擎）"
        onClick={handleOpen}
      >
        <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor"
          strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
          <path d="M6 2.5v5" />
          <path d="m3.5 5.5 2.5 2.5 2.5-2.5" />
          <path d="M2 10h8" />
        </svg>
      </button>

      <Modal open={open} title="AI 引擎更新（dsh）" onClose={() => setOpen(false)}>
        <div className="space-y-4">
          {phase.kind === 'checking' && (
            <StatusLine spinner text="正在检查版本…" />
          )}

          {phase.kind === 'error' && (
            <div className="space-y-3">
              <StatusLine text="检查失败" error />
              <div className="max-h-[32vh] overflow-auto text-[11.5px] text-nexus-muted leading-relaxed break-all bg-nexus-editor border border-nexus-border rounded-md p-2.5">
                {phase.message}
              </div>
              <div className="flex justify-end">
                <ActionButton onClick={() => void check()}>重试</ActionButton>
              </div>
            </div>
          )}

          {phase.kind === 'result' && (
            <>
              <div className="space-y-2 text-[12px]">
                <VersionRow label="当前版本" value={phase.info.dshFound ? phase.info.current ?? '未知' : '未安装'} />
                <VersionRow label="最新版本" value={phase.info.latest ?? '—'} highlight={phase.info.outdated} />
              </div>

              {!phase.info.dshFound ? (
                <p className="text-[11.5px] text-nexus-muted leading-relaxed">
                  未检测到 dsh（DeepSeek Harness CLI）。点下方按钮安装最新版，
                  安装进度将显示在 AI 面板上，装好后自动启动会话。
                </p>
              ) : phase.info.outdated ? (
                <p className="text-[11.5px] text-nexus-muted leading-relaxed">
                  有新版本可用。升级会先停止当前 AI 会话，安装进度将显示在 AI 面板上，
                  完成后自动重启会话。
                </p>
              ) : phase.info.latest ? (
                <p className="text-[11.5px] text-emerald-500/90">已是最新版本 ✓</p>
              ) : null}

              <div className="flex justify-end gap-2">
                {(phase.info.outdated || !phase.info.dshFound) && (
                  <ActionButton onClick={handleUpgrade}>
                    {installing ? `安装 ${phase.info.latest ?? '最新版'}` : `升级到 ${phase.info.latest ?? '最新版'}`}
                  </ActionButton>
                )}
                <button
                  className="px-3 py-1 text-[12px] text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 transition-colors"
                  onClick={() => setOpen(false)}
                >关闭</button>
              </div>
            </>
          )}
        </div>
      </Modal>
    </>
  );
}

/** 版本行（有新版时高亮最新值） */
function VersionRow({ label, value, highlight }: { label: string; value: string; highlight?: boolean }) {
  return (
    <div className="flex items-baseline justify-between">
      <span className="text-nexus-muted">{label}</span>
      <span className={highlight ? 'text-nexus-warning font-medium' : 'text-nexus-text'}>
        {value === '—' || value === '未知' || value.startsWith('v') ? value : `v${value}`}
      </span>
    </div>
  );
}

/** 状态行（spinner / 错误文案共用排版） */
function StatusLine({ text, spinner, error }: { text: string; spinner?: boolean; error?: boolean }) {
  return (
    <div className="flex items-center gap-2.5 text-[12px]">
      {spinner && (
        <span className="w-4 h-4 flex-shrink-0 border-2 border-nexus-accent/30 border-t-nexus-accent rounded-full animate-spin" />
      )}
      {!spinner && !error && <span className="w-4 h-4 flex-shrink-0 text-emerald-500 text-[13px] font-bold">✓</span>}
      {error && <span className="w-4 h-4 flex-shrink-0 text-nexus-error text-[13px] font-bold">✕</span>}
      <span className={error ? 'text-nexus-error' : 'text-nexus-text'}>{text}</span>
    </div>
  );
}

/** 主操作按钮（安装/升级/重试共用样式） */
function ActionButton({ onClick, children }: { onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      className="px-3 py-1 text-[12px] bg-nexus-accent/15 text-nexus-accent rounded-md hover:bg-nexus-accent/25 transition-colors"
      onClick={onClick}
    >{children}</button>
  );
}
