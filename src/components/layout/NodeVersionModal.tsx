import { useCallback, useEffect, useState } from 'react';
import { Modal } from '../ui/Modal';
import { nodeApi, type AvailableNodeVersions, type NodeRuntimeStatus } from '../../services/service';
import { showNotification } from '../ui/Toast';
import { reportError } from '../../utils/error';

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

  useEffect(() => {
    if (!open) return;
    void refresh();
    // 可安装列表要联网（几秒），打开面板时才拉
    setLoadingAvail(true);
    nodeApi.listAvailable()
      .then(setAvailable)
      .catch(() => setAvailable(null))
      .finally(() => setLoadingAvail(false));
  }, [open, refresh]);

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
        /* 没装 nvm 是正常情况，给引导而不是报错 */
        <div className="py-8 px-2 text-center space-y-2">
          <div className="text-[13px] text-nexus-text">没有检测到 nvm-windows</div>
          <div className="text-[12px] text-nexus-muted">{runtime.reason}</div>
          <div className="text-[11px] text-nexus-muted/70">
            这个面板依赖 nvm-windows 管理版本；装好后重开面板即可。
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
            ) : (
              <div className="text-[12px] text-nexus-muted py-2">
                {loadingAvail ? '' : '拿不到可安装列表（需要联网访问 nvm 配置的镜像）'}
              </div>
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
