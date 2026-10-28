import { useCallback, useEffect, useState } from 'react';
import {
  getAppPaths,
  getAutostartEnabled,
  openAppDir,
  setAutostartEnabled,
  type AppDirKind,
  type AppPaths,
} from '../../services/system';
import { resetLayoutToDefaults } from '../../stores/layoutStore';
import { notify } from '../../utils/notify';
import { reportError, toMessage } from '../../utils/error';

/**
 * 设置 > 通用：开机自启、数据与日志、布局。
 *
 * 这两项是"通用"里**现成能力就能支撑**的那部分：路径本来就有唯一来源（`lib.rs::data_dir()`），
 * 布局本来就有持久化。加到设置里不需要任何新的配置存储——而一个点进去空着的「通用」
 * 比没有设置更伤（用户点一次空的就不点了）。
 *
 * 刻意**不放进**这里的东西（等真需要再加）：主题（深色是硬编码的设计，没有第二套）、
 * 日志级别（现在是 `RUST_LOG` 环境变量，改它要动后端日志初始化）。
 */
export function GeneralSection() {
  const [paths, setPaths] = useState<AppPaths | null>(null);
  /**
   * 读取失败的原因（空 = 没失败）。
   *
   * 为什么要它：失败时 `paths` 仍是 null，而界面把 null 当"读取中…"——于是失败后
   * **永远**显示读取中，原因只在一条几秒后消失的 toast 里出现过一次（这个坑
   * `NodeVersionSection` 踩过，注释记在 UX-17）。
   */
  const [pathsError, setPathsError] = useState('');
  const [resetting, setResetting] = useState(false);
  /** 开机自启：null = 还没读到（读取中或失败，失败原因见 autostartError） */
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [autostartError, setAutostartError] = useState('');
  const [autostartBusy, setAutostartBusy] = useState(false);

  const loadPaths = useCallback(async () => {
    try {
      setPaths(await getAppPaths());
      setPathsError('');
    } catch (e: unknown) {
      setPaths(null);
      setPathsError(toMessage(e));
      reportError('读取应用目录失败', e);
    }
  }, []);

  useEffect(() => { void loadPaths(); }, [loadPaths]);

  /** 读开机自启状态（以后端与注册表的实际状态为准，不拿本地缓存猜） */
  const loadAutostart = useCallback(async () => {
    try {
      setAutostart(await getAutostartEnabled());
      setAutostartError('');
    } catch (e: unknown) {
      setAutostart(null);
      setAutostartError(toMessage(e));
      reportError('读取开机自启状态失败', e, { silent: true });
    }
  }, []);

  useEffect(() => { void loadAutostart(); }, [loadAutostart]);

  /**
   * 切换开机自启：写完后**回读系统实际状态**再更新开关。
   *
   * 为什么不直接把 next 塞进 state：写注册表可能被组策略/安全软件拦下，
   * "界面已开启、登录却没启动"是最难查的一类失败。回读失败时保留原值并就地报错。
   */
  const toggleAutostart = async (next: boolean) => {
    setAutostartBusy(true);
    setAutostartError('');
    try {
      await setAutostartEnabled(next);
      setAutostart(await getAutostartEnabled());
    } catch (e: unknown) {
      setAutostartError(toMessage(e));
      reportError('设置开机自启失败', e, { silent: true });
      try {
        setAutostart(await getAutostartEnabled());
      } catch {
        // 回读也失败：保留原值，界面上的错误已经说明这次操作没成功
      }
    } finally {
      setAutostartBusy(false);
    }
  };

  /**
   * 恢复默认布局。
   *
   * 副作用说清楚：只清面板尺寸/折叠状态（后端的 `reset_layout` 保留"当前选中的项目"），
   * 不碰项目、服务、模板与打开的工具。清理动作由 `resetLayoutToDefaults` 负责顺序
   * （先落库待写批次再删，理由见那里的注释）。
   */
  const handleResetLayout = async () => {
    setResetting(true);
    try {
      const removed = await resetLayoutToDefaults();
      notify({
        title: removed > 0 ? '已恢复默认布局' : '布局本来就是默认状态',
        description: removed > 0
          ? `清除了 ${removed} 项面板设置，当前选中的项目没有变`
          : '没有需要清除的面板设置',
      });
    } catch (e: unknown) {
      // 静默失败的表现是"按钮点了没反应"，所以这里必须报出来
      reportError('恢复默认布局失败', e);
    } finally {
      setResetting(false);
    }
  };

  return (
    <div className="space-y-5">
      <section>
        <SectionTitle>启动</SectionTitle>
        <div className="flex items-start gap-3 px-3 py-2.5 rounded-md border border-nexus-border bg-nexus-bg/30">
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2">
              <span className="text-[12px] text-nexus-text">开机自启</span>
              <span
                className={`text-[10px] leading-[16px] px-1.5 rounded-full transition-colors ${
                  autostart == null && autostartError
                    ? 'bg-nexus-error/10 text-nexus-error'
                    : autostart
                      ? 'bg-nexus-accent/15 text-nexus-accent'
                      : 'bg-nexus-hover/40 text-nexus-muted'
                }`}
              >
                {autostart == null
                  ? (autostartError ? '读取失败' : '读取中…')
                  : autostartBusy
                    ? '保存中…'
                    : autostart ? '已开启' : '已关闭'}
              </span>
            </div>
            <div className="text-[11px] text-nexus-muted mt-1 leading-relaxed">
              登录 Windows 时自动启动 Nexus。默认关闭，只写入当前用户启动项，不需要管理员权限。
            </div>
            {autostartError && (
              <div className="text-[11px] text-nexus-error leading-relaxed whitespace-pre-wrap break-all mt-1">
                {autostartError}
              </div>
            )}
          </div>
          {autostart == null && autostartError ? (
            <button
              className="px-2 py-1 text-[11px] text-nexus-accent border border-nexus-accent/40 rounded hover:bg-nexus-accent/10 flex-shrink-0 mt-0.5"
              onClick={() => {
                setAutostartError('');
                void loadAutostart();
              }}
            >重试</button>
          ) : (
            <Switch
              checked={autostart ?? false}
              disabled={autostart == null || autostartBusy}
              label="开机自启"
              onChange={(next) => void toggleAutostart(next)}
            />
          )}
        </div>
      </section>

      <section>
        <SectionTitle>数据与日志</SectionTitle>

        {paths ? (
          <div className="space-y-1.5">
            <PathRow label="数据目录" value={paths.data_dir} kind="data" />
            <PathRow label="日志文件" value={paths.log_file} kind="log" />
          </div>
        ) : pathsError ? (
          /* 失败态与"读取中"必须分开：否则用户会一直等一个不会来的结果 */
          <div className="space-y-2 py-2">
            <div className="text-[12px] text-nexus-muted">读取应用目录失败</div>
            <pre className="text-[11px] text-nexus-muted/80 font-mono whitespace-pre-wrap max-h-[90px] overflow-auto bg-nexus-bg/50 rounded p-2">
              {pathsError}
            </pre>
            <button
              className="px-2 py-1 text-[11px] text-nexus-accent border border-nexus-accent/40 rounded hover:bg-nexus-accent/10"
              onClick={() => void loadPaths()}
            >重试</button>
          </div>
        ) : (
          <div className="py-3 text-[12px] text-nexus-muted">读取中…</div>
        )}

        <p className="mt-2.5 text-[11px] text-nexus-muted/80 leading-relaxed">
          打包版没有控制台，出问题时这里就是唯一的现场：日志超过 5MB 会轮转为
          <span className="font-mono"> nexus.log.1</span>，前端错误也写在同一个文件里
          （行首带 <span className="font-mono">[frontend]</span> 标记）。
          要反馈问题时把最近的相关片段贴出来即可——上面这个路径可以直接选中复制，
          或者走「帮助 &gt; 报告问题」直接开 issue 页。
        </p>
      </section>

      <section>
        <SectionTitle>布局</SectionTitle>
        <div className="flex items-center gap-2 px-3 py-2 rounded-md border border-nexus-border bg-nexus-bg/30">
          <div className="flex-1 min-w-0">
            <div className="text-[12px] text-nexus-text">恢复默认布局</div>
            <div className="text-[11px] text-nexus-muted mt-0.5">
              把面板宽度、折叠状态与上下分栏高度调回初始值；项目、服务、配置与文本字号都不受影响
            </div>
          </div>
          <button
            className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40 flex-shrink-0"
            disabled={resetting}
            onClick={() => void handleResetLayout()}
          >{resetting ? '正在恢复…' : '恢复默认布局'}</button>
        </div>
      </section>
    </div>
  );
}

/** 小节标题（与 NodeVersionSection 的「已安装 / 可安装」同一套排版） */
function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider mb-2">
      {children}
    </div>
  );
}

/**
 * 滑动开关（`role="switch"` 的 button）。
 *
 * 为什么不用原生 checkbox：原生样式在各平台/主题下差异大，而且这一行的视觉重心
 * 应该在右侧那颗滑钮上。button 还能直接响应空格/回车，键盘可用性不打折。
 */
function Switch({
  checked, disabled, label, onChange,
}: {
  checked: boolean;
  disabled?: boolean;
  label: string;
  onChange: (checked: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative inline-flex h-[22px] w-[40px] flex-shrink-0 items-center rounded-full transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-nexus-accent/60 disabled:cursor-not-allowed disabled:opacity-40 ${
        checked ? 'bg-nexus-accent' : 'bg-nexus-border'
      }`}
    >
      <span
        aria-hidden="true"
        className={`inline-block h-[16px] w-[16px] rounded-full bg-white shadow-sm transition-transform duration-150 ${
          checked ? 'translate-x-[21px]' : 'translate-x-[3px]'
        }`}
      />
    </button>
  );
}

/**
 * 一行"路径 + 打开"。
 *
 * 路径用 `font-mono` + `truncate` + `title`：它可能很长（用户名长的机器上会超出这一行），
 * 截断是必然的，而**悬停能看到全串**是必须的——这一行的用途就是让人照着它去找文件。
 */
function PathRow({ label, value, kind }: { label: string; value: string; kind: AppDirKind }) {
  return (
    <div className="flex items-center gap-2 px-3 py-2 rounded-md border border-nexus-border bg-nexus-bg/30">
      <div className="flex-1 min-w-0">
        <div className="text-[11px] text-nexus-muted">{label}</div>
        <div className="text-[12px] text-nexus-text font-mono truncate" title={value}>{value}</div>
      </div>
      <button
        className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent flex-shrink-0"
        // 说清这一下会干什么：它拉起的是系统资源管理器，不是应用内的面板
        title={`在资源管理器中打开${label}`}
        onClick={() => void openAppDir(kind)}
      >打开</button>
    </div>
  );
}
