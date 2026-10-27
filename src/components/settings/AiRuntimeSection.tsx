import { useCallback, useEffect, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  aiService,
  AI_RUNTIME_PROGRESS_EVENT,
  type AiRuntimeStatus,
  type RuntimeProgress,
} from '../../services/aiService';
import { copyText } from '../../utils/clipboard';
import { notify } from '../../utils/notify';
import { reportError, toMessage } from '../../utils/error';
import { parseAiSessionMode, type AiSessionMode } from '../../utils/aiSession';
import { LAYOUT_KEYS, saveLayout, useLayoutStore } from '../../stores/layoutStore';
import { Modal } from '../ui/Modal';

/**
 * 设置 > AI 运行时。
 *
 * **为什么这一节必须存在**（它不是"锦上添花的设置项"）：AI 面板现在只用 Nexus 自带的
 * 那份 Node + dsh，自带运行时成了**唯一路径**——它坏了就等于 AI 没了。而"首次安装中途
 * 失败"（断网、下到一半、杀软锁文件）会留下一个半成品目录：既没有上一版可回退，
 * 也不满足"没装"的条件，任何自动机制都救不了，**只能清干净重来**——
 * 重来的入口只能是人按。这就是这个按钮组存在的全部理由。
 *
 * 反面边界同样要写死：**卸载只删 `~/.nexus/runtime`，`~/.dsh` 一个字节都不碰**
 * （那里是登录凭据、全部会话历史、浏览器 profile）。这句话必须出现在确认弹窗里——
 * 用户对"重置"的默认预期是"把 AI 清空重来"，而那个预期在这里会造成不可逆的数据丢失。
 */
export function AiRuntimeSection() {
  const [status, setStatus] = useState<AiRuntimeStatus | null>(null);
  const [loadError, setLoadError] = useState('');
  const [progress, setProgress] = useState<RuntimeProgress | null>(null);
  const [busy, setBusy] = useState(false);
  /** 非 null = 正开着确认弹窗（'uninstall' | 'reinstall'） */
  const [confirming, setConfirming] = useState<'uninstall' | 'reinstall' | null>(null);
  /** 手动指定（A 方案）那块是否展开 */
  const [showManual, setShowManual] = useState(false);
  const [nodeExe, setNodeExe] = useState('');
  const [dshPath, setDshPath] = useState('');
  const [manualBusy, setManualBusy] = useState(false);
  /** 手动指定保存失败的原因（就地显示——这个失败**不能**只飘一个 toast 就没了） */
  const [manualError, setManualError] = useState('');

  // 打开方式：持久化在 layout KV 表（与字号同一类"非几何设置"），默认新建会话
  const sessionMode = parseAiSessionMode(
    useLayoutStore((s) => s.values?.[LAYOUT_KEYS.aiSessionMode]),
  );
  const setSessionMode = (mode: AiSessionMode) => saveLayout({ [LAYOUT_KEYS.aiSessionMode]: mode });

  const load = useCallback(async () => {
    try {
      setStatus(await aiService.runtimeStatus());
      setLoadError('');
    } catch (e: unknown) {
      setStatus(null);
      setLoadError(toMessage(e));
      reportError('读取 AI 运行时状态失败', e);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let alive = true;
    void listen<RuntimeProgress>(AI_RUNTIME_PROGRESS_EVENT, (ev) => setProgress(ev.payload))
      .then((fn) => { if (alive) unlisten = fn; else fn(); })
      .catch((e) => reportError('订阅 AI 运行时进度失败', e));
    return () => { alive = false; unlisten?.(); };
  }, []);

  const runInstall = async () => {
    setBusy(true);
    setProgress(null);
    try {
      setStatus(await aiService.runtimeInstall());
      notify({ variant: 'success', title: 'AI 运行时就绪' });
    } catch (e: unknown) {
      // 失败体可能是结构化对象（RuntimeFailure），`toMessage` 处理不了——把摘要与原话都留下
      const detail = e != null && typeof e === 'object' && 'summary' in e
        ? `${(e as { step?: string }).step ?? ''}：${(e as { summary?: string }).summary ?? ''}\n${
            (e as { raw?: string }).raw ?? ''}`.trim()
        : toMessage(e);
      notify({ variant: 'error', title: 'AI 运行时准备失败', description: detail, duration: 10000 });
      console.error('AI 运行时安装失败:', e);
    } finally {
      setBusy(false);
      setProgress(null);
      await load();
    }
  };

  const saveManual = async () => {
    setManualBusy(true);
    setManualError('');
    try {
      setStatus(await aiService.runtimeSetManual(nodeExe.trim(), dshPath.trim()));
      notify({ variant: 'success', title: '已切换到手动指定的运行时' });
    } catch (e: unknown) {
      setManualError(toMessage(e));
    } finally {
      setManualBusy(false);
    }
  };

  const runUninstall = async () => {
    setBusy(true);
    try {
      await aiService.runtimeUninstall();
      notify({ variant: 'success', title: 'AI 运行时已卸载' });
    } catch (e: unknown) {
      notify({ variant: 'error', title: '卸载失败', description: toMessage(e), duration: 10000 });
    } finally {
      setBusy(false);
      await load();
    }
  };

  return (
    <div className="space-y-6">
      <section>
        <SectionTitle>AI 运行时</SectionTitle>
        <div className="px-3 py-2.5 rounded-md border border-nexus-border bg-nexus-bg/30 space-y-2">
          {loadError ? (
            <div className="text-[12px] text-nexus-error leading-relaxed break-all">
              读取运行时状态失败：{loadError}
            </div>
          ) : status == null ? (
            <div className="text-[12px] text-nexus-muted">读取中…</div>
          ) : (
            <>
              <div className="flex items-baseline gap-2">
                <span className="text-[12px] text-nexus-text">
                  {status.source === 'manual' ? '手动指定的 Node + dsh' : '自带 Node + dsh'}
                </span>
                <span className={`text-[11px] ${status.ready ? 'text-emerald-500/90' : 'text-nexus-muted'}`}>
                  {status.ready ? '已就绪' : '未准备'}
                </span>
              </div>

              {/* 同一行的次要标签同字号，只用颜色弱化（见 nexus-ui-label-same-font-size） */}
              {status.ready && (
                <div className="text-[11.5px] text-nexus-muted">
                  {/* 手动那份是用户自己的 Node，Nexus 不去猜它的版本号——那就别显示
                      「Node —」（一个没有意义的破折号只会让人以为读取出错了） */}
                  {status.source === 'manual' ? 'dsh ' : 'Node '}
                  {status.source === 'manual'
                    ? (status.dsh_version ?? '—')
                    : `${status.node_version ?? '—'} · dsh ${status.dsh_version ?? '—'}`}
                  {' · 占用 '}{formatBytes(status.size_bytes)}
                </div>
              )}

              {status.problem && (
                <div className="text-[11.5px] text-nexus-warning leading-relaxed break-all">
                  装了但跑不起来：{status.problem}
                </div>
              )}

              <div className="flex items-center gap-2 min-w-0">
                <span className="text-[11px] text-nexus-muted truncate flex-1 min-w-0" title={status.path}>
                  {status.path}
                </span>
                <button
                  className="px-2 py-0.5 text-[11px] text-nexus-muted border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent flex-shrink-0"
                  onClick={() => void copyText(status.path, '路径')}
                >复制路径</button>
              </div>

              {busy ? (
                <RuntimeProgressInline progress={progress} steps={status.steps} />
              ) : (
                <div className="flex items-center gap-2 pt-0.5">
                  {/* 手动指定时**不给「重新安装」**：那份不是 Nexus 装的，重装指的是自带那份，
                      而手动指定优先于它——点了会白下 300MB 且看不出任何变化。这条路的出口是下面的「清除」 */}
                  {status.source !== 'manual' && (
                    <button
                      className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent"
                      // 重装走确认弹窗（有代价）；首次准备没有可失去的东西，直接开跑，不做假选择
                      onClick={() => { if (status.ready) setConfirming('reinstall'); else void runInstall(); }}
                    >{status.ready ? '重新安装' : '准备 AI 运行时（下载约 300MB）'}</button>
                  )}
                  {status.source === 'manual' && (
                    <button
                      className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
                      disabled={manualBusy}
                      onClick={() => {
                        setManualBusy(true);
                        setManualError('');
                        aiService.runtimeClearManual()
                          .then(setStatus)
                          .catch((e: unknown) => setManualError(toMessage(e)))
                          .finally(() => setManualBusy(false));
                      }}
                    >清除手动指定，回到自带那份</button>
                  )}
                  {status.ready && status.source !== 'manual' && (
                    <button
                      className="px-2 py-1 text-[11px] text-nexus-muted border border-nexus-border rounded hover:border-nexus-error hover:text-nexus-error"
                      onClick={() => setConfirming('uninstall')}
                    >卸载</button>
                  )}
                </div>
              )}

              <p className="text-[11px] text-nexus-muted leading-relaxed pt-0.5">
                {status.source === 'manual' ? (
                  <>
                    当前用的是<span className="text-nexus-text">你手动指定的那份</span>
                    ——它优先于自带运行时。路径失效或被改动时 AI 会直接报错（不静默回退），
                    那时点上面的「清除手动指定」即可回到自带那份。
                  </>
                ) : (
                  <>
                    AI 面板用的是 Nexus 自带的这份 Node，
                    <span className="text-nexus-text">与你机器上的 nvm / Node 版本无关</span>
                    ——切 nvm、卸载某个 Node 版本都不会影响 AI。
                  </>
                )}
              </p>

              {/* ── 手动指定（A 方案）──────────────────────────────
                  内网 / 下载不通时的逃生口。默认收起：能用下载的绝大多数人不需要它，
                  摆在外面只会让"我该填什么"变成一个多余的问题。 */}
              <div className="pt-1.5 border-t border-nexus-border/60">
                <button
                  className="text-[11px] text-nexus-muted hover:text-nexus-text underline underline-offset-2"
                  onClick={() => setShowManual((v) => !v)}
                >
                  {showManual ? '收起手动指定' : '手动指定 Node 与 dsh（内网 / 下载不通时用）'}
                </button>

                {showManual && (
                  <div className="mt-2 space-y-2">
                    <p className="text-[11px] text-nexus-muted leading-relaxed">
                      机器上已经有能跑 dsh 的 Node 时，填这两条路径就能跳过那 300MB 下载。
                      保存前会<span className="text-nexus-text">当场跑一次</span>
                      <span className="font-mono"> dsh --version </span>验证，跑不起来不会存。
                    </p>
                    <ManualField
                      label="node.exe 完整路径"
                      placeholder="C:\Program Files\nodejs\node.exe"
                      value={nodeExe}
                      onChange={setNodeExe}
                      disabled={manualBusy}
                    />
                    <ManualField
                      label="dsh 路径（包目录或 lib\bin.js）"
                      placeholder="C:\Users\…\node_modules\@deepseek-ai\dsh"
                      value={dshPath}
                      onChange={setDshPath}
                      disabled={manualBusy}
                    />
                    {manualError && (
                      <div className="text-[11px] text-nexus-error leading-relaxed whitespace-pre-wrap break-all">
                        {manualError}
                      </div>
                    )}
                    <div className="flex items-center gap-2">
                      <button
                        className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
                        disabled={manualBusy || !nodeExe.trim() || !dshPath.trim()}
                        onClick={() => void saveManual()}
                      >{manualBusy ? '正在验证…' : '保存并验证'}</button>
                      {status.has_manual && (
                        <button
                          className="px-2 py-1 text-[11px] text-nexus-muted border border-nexus-border rounded hover:border-nexus-text hover:text-nexus-text"
                          disabled={manualBusy}
                          onClick={() => {
                            setManualBusy(true);
                            setManualError('');
                            aiService.runtimeClearManual()
                              .then(setStatus)
                              .catch((e: unknown) => setManualError(toMessage(e)))
                              .finally(() => setManualBusy(false));
                          }}
                        >清除手动指定，回到自带那份</button>
                      )}
                    </div>
                  </div>
                )}
              </div>
            </>
          )}
        </div>
      </section>

      {/* ── AI 会话：打开面板时是新建还是接上上次 ───────────────────
          默认新建（保持历史行为）：稳定端口 + 不清理记忆会让 dsh 自动恢复上次
          会话，对"每次都想开新对话"的用户反而是意外。 */}
      <section>
        <SectionTitle>AI 会话</SectionTitle>
        <div className="px-3 py-2.5 rounded-md border border-nexus-border bg-nexus-bg/30 space-y-2">
          <SessionModeOption
            checked={sessionMode === 'new'}
            onSelect={() => setSessionMode('new')}
            title="每次打开新建会话（默认）"
            detail="打开 AI 面板时停在空白对话页；历史会话仍在侧栏里，不会被删除。"
          />
          <SessionModeOption
            checked={sessionMode === 'resume'}
            onSelect={() => setSessionMode('resume')}
            title="打开上次使用的会话"
            detail="自动接上该项目最近一次真正聊过的会话；空白会话与已归档会话会被跳过。"
          />
          <p className="text-[11px] text-nexus-muted leading-relaxed pt-0.5">
            修改后在下一次启动/重建 AI 会话时生效（关闭面板只是隐藏，当前对话会保留）。
          </p>
        </div>
      </section>

      {/* 确认弹窗：这是本面板唯一该用弹窗的地方——用户马上要做一件有代价的事（300MB + 几分钟），
          而且还有回头路可走。失败之后没有回头路，那种场景一律走就地卡片，不弹窗 */}
      <Modal
        open={confirming !== null}
        title={confirming === 'uninstall' ? '卸载 AI 运行时' : '重新安装 AI 运行时'}
        onClose={() => setConfirming(null)}
      >
        <div className="space-y-3 text-[12px] text-nexus-text leading-relaxed">
          {confirming === 'uninstall' ? (
            <>
              <p>将删除 AI 运行时（约 {formatBytes(status?.size_bytes ?? 0)}），下次使用 AI 需要重新下载。</p>
              <p className="text-nexus-muted">
                <span className="text-nexus-text">不会</span>删除你的登录信息与全部 AI 会话历史
                ——它们在 <span className="font-mono">~/.dsh</span> 下，这里一个字节都不动。
              </p>
            </>
          ) : (
            <>
              <p>会先停止当前的 AI 会话，然后删掉并重新下载约 300MB。</p>
              <p className="text-nexus-muted">
                用在"装了但跑不起来"或下载中途失败留下半成品的情况。
                同样<span className="text-nexus-text">不会</span>碰 <span className="font-mono">~/.dsh</span>
                （登录信息与会话历史）。
              </p>
            </>
          )}
          <div className="flex justify-end gap-2 pt-1">
            <button
              className="px-3 py-1 text-[12px] text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50"
              onClick={() => setConfirming(null)}
            >取消</button>
            <button
              className={`px-3 py-1 text-[12px] rounded-md ${
                confirming === 'uninstall'
                  ? 'bg-nexus-error/15 text-nexus-error hover:bg-nexus-error/25'
                  : 'bg-nexus-accent/15 text-nexus-accent hover:bg-nexus-accent/25'
              }`}
              onClick={() => {
                const which = confirming;
                setConfirming(null);
                void (which === 'uninstall' ? runUninstall() : runInstall());
              }}
            >{confirming === 'uninstall' ? '卸载' : '重新安装'}</button>
          </div>
        </div>
      </Modal>
    </div>
  );
}

/** 手动指定的输入行（标签 + 单行输入） */
function ManualField({
  label, placeholder, value, onChange, disabled,
}: {
  label: string;
  placeholder: string;
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  return (
    <label className="block space-y-1">
      <span className="text-[11px] text-nexus-muted">{label}</span>
      <input
        className="w-full px-2 py-1 text-[11.5px] font-mono text-nexus-text bg-nexus-editor border border-nexus-border rounded outline-none focus:border-nexus-accent disabled:opacity-50"
        placeholder={placeholder}
        value={value}
        spellCheck={false}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      />
    </label>
  );
}

/** 安装中的就地进度（设置面板里的版本，尺寸比 AI 面板里那套小） */
function RuntimeProgressInline({ progress, steps }: { progress: RuntimeProgress | null; steps: string[] }) {
  const current = progress?.step_index ?? 0;
  return (
    <div className="space-y-1.5 pt-0.5">
      {steps.map((label, i) => (
        <div key={label} className="flex items-start gap-2 text-[11.5px] leading-snug">
          <span className={`mt-[3px] w-2 h-2 flex-shrink-0 rounded-full ${
            i < current ? 'bg-emerald-500' : i === current ? 'bg-nexus-accent animate-pulse' : 'bg-nexus-border'
          }`} />
          <span className={i === current ? 'text-nexus-text' : 'text-nexus-muted'}>{label}</span>
        </div>
      ))}
      <div className="text-[11px] text-nexus-muted tabular-nums">{progress?.detail ?? '正在开始…'}</div>
    </div>
  );
}

/** 单选行：标题 + 一句解释（点击整行可选中，命中区域比那个小圆点大得多） */
function SessionModeOption({
  checked, onSelect, title, detail,
}: {
  checked: boolean;
  onSelect: () => void;
  title: string;
  detail: string;
}) {
  return (
    <label className="flex items-start gap-2 cursor-pointer rounded px-1 py-1 hover:bg-nexus-hover/30 transition-colors">
      <input
        type="radio"
        name="ai-session-mode"
        checked={checked}
        onChange={onSelect}
        className="mt-[3px] flex-shrink-0 accent-[#74ade8]"
      />
      <span className="min-w-0">
        <span className="block text-[12px] text-nexus-text">{title}</span>
        <span className="block text-[11px] text-nexus-muted leading-relaxed">{detail}</span>
      </span>
    </label>
  );
}

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider mb-2">{children}</div>
  );
}

function formatBytes(n: number): string {
  if (n <= 0) return '0 MB';
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
