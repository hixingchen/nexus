import { useCallback, useEffect, useState } from 'react';
import { Modal } from '../ui/Modal';
import { nodeApi, type AvailableNodeVersions, type NodeRuntimeStatus } from '../../services/service';
import { showNotification } from '../ui/Toast';
import { reportError, toMessage } from '../../utils/error';

/**
 * 一键配置用的国内镜像（与用户本机 `settings.txt` 里那两条一致）。
 *
 * 为什么面板要管这件事：`nvm list available` 和 `nvm install` 都要去 `node_mirror`
 * 取索引与安装包，默认是 `https://nodejs.org/dist/`——国内机器上这一步经常直接失败，
 * 而失败信息是英文的传输层报错，用户很难自己联想到"要配镜像"。
 */
const NODE_MIRROR = 'https://npmmirror.com/mirrors/node/';
const NPM_MIRROR = 'https://npmmirror.com/mirrors/npm/';

/**
 * Node 版本管理面板（nvm-windows 的 GUI）。
 *
 * **它是独立工具，不参与服务配置**：这里改的是全局状态（`nvm use` 动的是软链），
 * 与"某个服务该用哪个 Node"是两件事，别把两者混在一个入口里。
 *
 * 三条刻意的设计：
 * 1. **成败看 `ok`（退出码），输出原文照贴**——nvm 的提示是给人看的，措辞会随版本变，
 *    我们转述不如直接给它的话
 * 2. 「设为当前」会改**全局软链**（所有终端里的 node 都跟着变），按钮上必须说清楚
 * 3. **卸载正在使用的版本由我们自己拦**——nvm 不管这事（它反而会先删软链再删目录，
 *    见 handleUninstall），所以"为什么不行、接下来怎么办"都得由我们说出来
 */

type Action = 'install' | 'uninstall' | 'use';

const ACTION_LABEL: Record<Action, string> = {
  install: '安装',
  uninstall: '卸载',
  use: '设为当前',
};

interface Props {
  open: boolean;
  onClose: () => void;
  /**
   * 某次操作成功、并已重新读过状态之后调用。
   *
   * 调用方（左上角那个按钮）拿它刷新自己显示的版本号——**不能等面板关闭再刷**：
   * 用户切完版本后多半还开着面板继续看列表，那时候按钮上已经是旧值了
   * （用户报过"点了设为当前界面状态没变化"）。
   */
  onChanged?: () => void;
}

export function NodeVersionModal({ open, onClose, onChanged }: Props) {
  const [runtime, setRuntime] = useState<NodeRuntimeStatus | null>(null);
  /** 可安装版本：联网拉，失败保持 null（不影响已装列表的使用） */
  const [available, setAvailable] = useState<AvailableNodeVersions | null>(null);
  const [loadingAvail, setLoadingAvail] = useState(false);
  /**
   * 拉列表失败时的原文（nvm 的原话，如 "Get https://nodejs.org/dist/index.json: ..."）。
   *
   * 之前这里只留一句"拿不到可安装列表"，把后端带回来的原因扔了——用户看到的是泛泛一句，
   * 排不了障，我们也只能猜（用户就是这么撞上的）。失败原因必须露出来。
   */
  const [availError, setAvailError] = useState('');
  /** 正在写镜像配置 */
  const [mirrorBusy, setMirrorBusy] = useState(false);
  /** 正在执行的操作（同时只允许一个：nvm 自己也不是并发安全的） */
  const [busy, setBusy] = useState<{ version: string; action: Action } | null>(null);
  /** 最近一次命令的输出原文，失败时是排查的唯一线索 */
  const [lastOutput, setLastOutput] = useState('');

  const refresh = useCallback(async () => {
    try {
      setRuntime(await nodeApi.getRuntime());
    } catch (e: unknown) {
      reportError('读取 Node 运行时状态失败', e);
    }
  }, []);

  /** 拉可安装版本（`nvm list available`）。失败时把 nvm 的原话留下，别只留一句状态 */
  const loadAvailable = useCallback(async () => {
    setLoadingAvail(true);
    setAvailError('');
    try {
      setAvailable(await nodeApi.listAvailable());
    } catch (e: unknown) {
      setAvailable(null);
      setAvailError(toMessage(e));
    } finally {
      setLoadingAvail(false);
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    void refresh();
    // 可安装列表要联网（几秒），打开面板时才拉
    void loadAvailable();
  }, [open, refresh, loadAvailable]);

  /**
   * 一键换成国内镜像（写 nvm 自己的 settings.txt，所以只由用户点触发）。
   *
   * 新装的 nvm 默认去 nodejs.org 取版本索引——国内机器上这就是"拿不到可安装列表"的
   * 头号原因；配完立刻重拉一次，用户能直接看到结果。
   */
  const applyMirror = async () => {
    setMirrorBusy(true);
    try {
      const res = await nodeApi.setMirrors(NODE_MIRROR, NPM_MIRROR);
      if (!res.ok) {
        showNotification({
          variant: 'error',
          title: '配置镜像失败',
          description: res.output.slice(0, 300),
          duration: 8000,
        });
      } else {
        showNotification({ title: '已配置 npmmirror 镜像', description: '重新拉取可安装列表…' });
        await loadAvailable();
      }
    } catch (e: unknown) {
      reportError('配置镜像失败', e);
    } finally {
      setMirrorBusy(false);
    }
  };

  const run = async (version: string, action: Action) => {
    setBusy({ version, action });
    setLastOutput('');
    try {
      const res = action === 'install'
        ? await nodeApi.install(version)
        : action === 'uninstall'
          ? await nodeApi.uninstall(version)
          : await nodeApi.use(version);

      setLastOutput(res.output);
      if (res.ok) {
        showNotification({ title: `已${ACTION_LABEL[action]} ${version}` });
        await refresh();
        // 按钮上的版本号跟着变（只有 use 会改它，但重读一次的成本可以忽略）
        onChanged?.();
      } else {
        // 失败：把 nvm 的原话贴出来（"正在使用中不许卸载"这类判断只有它做得准）
        showNotification({
          variant: 'error',
          title: `${ACTION_LABEL[action]} ${version} 失败`,
          description: res.output.slice(0, 300),
          duration: 8000,
        });
      }
    } catch (e: unknown) {
      reportError(`${ACTION_LABEL[action]} ${version} 失败`, e);
    }
    setBusy(null);
  };

  const disabled = busy !== null;

  /**
   * 安装 nvm 的两段状态：下载 → 向导。
   *
   * 分开是因为**界面要说清"在下载"还是"在等你操作"**：下载要一两分钟（还可能慢），
   * 而向导开着的时候用户得去点它——两件事合成一句"处理中"，用户就不知道该干嘛了。
   */
  const [installing, setInstalling] = useState<'idle' | 'download' | 'wizard'>('idle');

  const handleInstallNvm = async () => {
    setInstalling('download');
    try {
      await nodeApi.downloadNvmInstaller();
      setInstalling('wizard');
      const res = await nodeApi.runNvmInstaller();
      if (!res.ok) {
        // 用户点了取消也走这里：说清"什么都没改"，并给一条重来的路
        showNotification({
          variant: 'warning',
          title: '安装向导没有走完',
          description: `退出码 ${res.code ?? '未知'}。如果取消了安装，可以再点一次按钮——安装器已经在本机，不用重新下载。`,
          duration: 8000,
        });
      }
    } catch (e) {
      reportError('安装 nvm-windows 失败', e);
    } finally {
      setInstalling('idle');
      // 成败都要重看一次：装好了这里就能列出版本（环境变量读不到时后端会去读注册表，
      // 所以不必重启应用）
      await refresh();
    }
  };

  /**
   * 卸载：**当前版本不发命令**，直接把原因说清楚。
   *
   * 起初以为"nvm 会拒绝卸载在用的版本"，实测不是（nvm-windows 1.2.2 会先删掉
   * NVM_SYMLINK 软链、再删版本目录，所有终端里的 node/npm 一起消失）。所以拦住它的
   * 是我们自己——后端 `uninstall_node_version` 里有同一道守卫，这里点之前就说清楚，
   * 用户不用先吃一个错误提示才知道为什么。
   */
  const handleUninstall = (version: string, isCurrent: boolean) => {
    if (isCurrent) {
      showNotification({
        variant: 'warning',
        title: '这是当前正在使用的版本',
        description: `所有终端里的 node 现在都是 ${version}。先点别的版本的「设为当前」，再回来卸载。`,
        duration: 6000,
      });
      return;
    }
    void run(version, 'uninstall');
  };

  return (
    <Modal open={open} title="Node 版本" onClose={onClose} width="560px">
      {!runtime ? (
        <div className="py-10 text-center text-[12px] text-nexus-muted">读取中…</div>
      ) : !runtime.available ? (
        /* 没装 nvm 是正常情况，给引导而不是报错——而且要**直接给装的入口**：
           "你自己去查这东西怎么下"是这一步最不该留给用户的事（用户原话） */
        <div className="py-8 px-2 text-center space-y-3">
          <div className="text-[13px] text-nexus-text">没有检测到 nvm-windows</div>
          <div className="text-[12px] text-nexus-muted">{runtime.reason}</div>

          <div className="flex items-center justify-center gap-2 pt-1">
            <button
              className="px-3 py-1.5 text-[12px] bg-nexus-accent/15 text-nexus-accent rounded-md hover:bg-nexus-accent/25 transition-colors disabled:opacity-50"
              disabled={installing !== 'idle'}
              onClick={() => void handleInstallNvm()}
            >
              {installing === 'download' ? '正在下载安装器…'
                : installing === 'wizard' ? '安装向导已打开…'
                  : '下载并安装 nvm-windows'}
            </button>
            <button
              className="px-3 py-1.5 text-[12px] text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 transition-colors disabled:opacity-50"
              disabled={installing !== 'idle'}
              onClick={() => void refresh()}
            >重新检测</button>
          </div>

          {installing === 'wizard' && (
            <div className="text-[11.5px] text-nexus-accent/90">
              安装向导已经打开，按它走完就行；结束后这里会自动刷新。
            </div>
          )}

          <div className="text-[11px] text-nexus-muted/70 leading-relaxed max-w-[430px] mx-auto">
            上面那个按钮会从官方 release 下载 nvm-windows 安装器（约 5.6 MB），再弹出官方安装向导
            ——它会问 nvm 目录与软链位置。安装器要求管理员权限，会弹一次 UAC。
          </div>
          <div className="text-[11px] text-nexus-warning/90 leading-relaxed max-w-[430px] mx-auto">
            它是未签名的社区构建，Windows 可能提示「已保护你的电脑」——要点「更多信息 → 仍要运行」。
          </div>
        </div>
      ) : (
        <div className="space-y-4">
          {/* 已安装 */}
          <section>
            <div className="flex items-center justify-between mb-2">
              <span className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider">已安装</span>
              <span className="text-[10px] text-nexus-muted/60 font-mono truncate max-w-[300px]" title={runtime.root}>
                {runtime.root}
              </span>
            </div>
            {runtime.installed.length === 0 ? (
              <div className="text-[12px] text-nexus-muted py-3 text-center">还没有装任何版本</div>
            ) : (
              <div className="space-y-1">
                {runtime.installed.map(v => {
                  const isCurrent = v === runtime.current;
                  const busyHere = busy?.version === v;
                  return (
                    <div
                      key={v}
                      className={`flex items-center gap-2 px-3 py-2 rounded-md border ${
                        isCurrent ? 'border-nexus-accent/40 bg-nexus-accent/10' : 'border-nexus-border bg-nexus-bg/30'
                      }`}
                    >
                      <span className="flex-1 text-[13px] text-nexus-text font-mono">{v}</span>
                      {isCurrent && (
                        <span className="text-[10px] px-1.5 py-0.5 rounded bg-nexus-accent/20 text-nexus-accent">当前</span>
                      )}
                      {busyHere && <span className="text-[11px] text-nexus-muted">{ACTION_LABEL[busy.action]}中…</span>}
                      {!isCurrent && (
                        <button
                          className="px-2 py-1 text-[11px] text-nexus-accent border border-nexus-accent/40 rounded hover:bg-nexus-accent/10 disabled:opacity-40"
                          disabled={disabled}
                          // 说清副作用：这一下会改全局软链，用户在所有终端里看到的 node 都会变
                          title="切换后，你在所有终端里看到的 node 都变成这个版本"
                          onClick={() => void run(v, 'use')}
                        >设为当前</button>
                      )}
                      <button
                        className="px-2 py-1 text-[11px] text-nexus-error border border-nexus-error/30 rounded hover:bg-nexus-error/10 disabled:opacity-40"
                        disabled={disabled}
                        // 当前版本照样可点，点了给解释而不是执行（见 handleUninstall）。
                        // 不置灰：看着不能点、其实能点会自相矛盾，而"为什么不行"只能由我们说
                        title={isCurrent ? '正在使用的版本：先切换到别的版本再卸载' : undefined}
                        onClick={() => handleUninstall(v, isCurrent)}
                      >卸载</button>
                    </div>
                  );
                })}
              </div>
            )}
          </section>

          {/* 可安装 */}
          <section>
            <div className="flex items-center gap-2 mb-2">
              <span className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider">可安装</span>
              {loadingAvail && <span className="text-[11px] text-nexus-muted">查询中…</span>}
            </div>
            {available ? (
              <div className="space-y-2 max-h-[190px] overflow-auto pr-1">
                {([
                  ['LTS', available.lts],
                  ['当前线', available.current],
                  ['旧稳定版', available.old_stable],
                  ['旧非稳定版', available.old_unstable],
                ] as const).map(([label, list]) => list.length > 0 && (
                  <div key={label}>
                    <div className="text-[11px] text-nexus-muted mb-1">{label}</div>
                    <div className="flex flex-wrap gap-1">
                      {list.map(v => {
                        const installed = runtime.installed.includes(`v${v}`);
                        return (
                          <button
                            key={v}
                            className={`px-2 py-0.5 text-[11px] font-mono rounded border transition-colors ${
                              installed
                                ? 'border-nexus-border/40 text-nexus-muted/50 cursor-default'
                                : 'border-nexus-border text-nexus-text hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40'
                            }`}
                            disabled={installed || disabled}
                            title={installed ? '已安装' : `安装 v${v}（要从镜像下载几十兆，请稍候）`}
                            onClick={() => void run(`v${v}`, 'install')}
                          >{v}</button>
                        );
                      })}
                    </div>
                  </div>
                ))}
              </div>
            ) : availError ? (
              /* 失败态一直留着（只在首次加载、还没结果时留白）：重试按钮自己显示"重试中…"，
                 否则一次很快就失败的请求看上去像"点了没反应"（用户报过） */
              <div className="text-[12px] text-nexus-muted py-2 space-y-2">
                <div>拿不到可安装列表（nvm 要去配置的镜像取版本索引）</div>
                {/* nvm 的原话：网络报错、镜像 404、代理问题全在这里，不贴出来就只能猜 */}
                <pre className="text-[11px] text-nexus-muted/80 font-mono whitespace-pre-wrap max-h-[90px] overflow-auto bg-nexus-bg/50 rounded p-2">
                  {availError}
                </pre>
                <div className="flex items-center gap-2">
                  <button
                    className="px-2 py-1 text-[11px] text-nexus-accent border border-nexus-accent/40 rounded hover:bg-nexus-accent/10 disabled:opacity-40"
                    disabled={disabled || mirrorBusy || loadingAvail}
                    onClick={() => void loadAvailable()}
                  >{loadingAvail ? '重试中…' : '重试'}</button>
                  <button
                    className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
                    disabled={disabled || mirrorBusy || loadingAvail}
                    // 说清副作用：这一下会改 nvm 自己的 settings.txt（默认源是 nodejs.org）
                    title="把 nvm 的 node/npm 镜像改成 npmmirror 并重拉列表（写 nvm 自己的 settings.txt）"
                    onClick={() => void applyMirror()}
                  >{mirrorBusy ? '正在配置…' : '改用 npmmirror 镜像'}</button>
                </div>
              </div>
            ) : (
              // 首次加载：头部有"查询中…"，这里不必再说什么
              <div className="text-[12px] text-nexus-muted py-2" />
            )}
          </section>

          {/* 下载几十兆时没有任何提示的话，用户会以为卡死 */}
          {busy?.action === 'install' && (
            <div className="text-[11.5px] text-nexus-warning">
              正在安装 {busy.version}…这一步要从镜像下载几十兆，慢的时候要一两分钟，请勿关闭窗口。
            </div>
          )}

          {/* 命令原文：成功时是记录，失败时是唯一的线索 */}
          {lastOutput && (
            <section>
              <div className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider mb-1">输出</div>
              <pre className="text-[11px] text-nexus-muted font-mono whitespace-pre-wrap max-h-[120px] overflow-auto bg-nexus-bg/50 rounded p-2">
                {lastOutput}
              </pre>
            </section>
          )}
        </div>
      )}
    </Modal>
  );
}
