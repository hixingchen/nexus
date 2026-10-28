import { useCallback, useEffect, useState } from 'react';
import { bootApi, type BootRunnerStatus } from '../../services/boot';
import { notify } from '../../utils/notify';
import { reportError, toMessage } from '../../utils/error';

/**
 * 设置 > 服务开机启动。
 *
 * 这个分类只管**启动器本身**（计划任务 + runner 的安装/状态/启停）；
 * 具体哪个服务要开机启动，在服务编辑面板里逐个勾选。
 *
 * 状态一律以**回读计划任务 + runner 心跳**为准，不拿本地缓存猜（铁律 21）：
 * 界面显示"已安装"而任务实际指向旧 exe，是这里最需要避免的谎报。
 */
export function ServiceBootSection() {
  const [status, setStatus] = useState<BootRunnerStatus | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState('');

  const load = useCallback(async () => {
    try {
      setStatus(await bootApi.getStatus());
      setError('');
    } catch (e: unknown) {
      setStatus(null);
      setError(toMessage(e));
      reportError('读取服务开机启动状态失败', e, { silent: true });
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const run = async (label: string, fn: () => Promise<BootRunnerStatus>, okText: string) => {
    setBusy(label);
    setError('');
    try {
      const next = await fn();
      setStatus(next);
      notify({ title: okText, description: next.message });
    } catch (e: unknown) {
      setError(toMessage(e));
      reportError(`${label}失败`, e, { silent: true });
    } finally {
      setBusy('');
    }
  };

  const stateChip = (() => {
    if (!status) return { text: error ? '读取失败' : '读取中…', cls: 'bg-nexus-hover/40 text-nexus-muted' };
    switch (status.state) {
      case 'running':
        return { text: '运行中', cls: 'bg-nexus-success/15 text-nexus-success' };
      case 'installed_stopped':
        return { text: '已安装 · 未运行', cls: 'bg-nexus-warning/15 text-nexus-warning' };
      case 'unreachable':
        return { text: '无响应', cls: 'bg-nexus-error/15 text-nexus-error' };
      default:
        return { text: '未安装', cls: 'bg-nexus-hover/40 text-nexus-muted' };
    }
  })();

  return (
    <div className="space-y-5">
      <section>
        <SectionTitle>服务开机启动（登录前）</SectionTitle>
        <div className="px-3 py-2.5 rounded-md border border-nexus-border bg-nexus-bg/30 space-y-2">
          <div className="flex items-center gap-2">
            <span className="text-[12px] text-nexus-text flex-1">开机启动器</span>
            <span className={`text-[10px] leading-[16px] px-1.5 rounded-full ${stateChip.cls}`}>
              {stateChip.text}
            </span>
            <button
              className="px-2 py-1 text-[11px] text-nexus-muted border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
              disabled={!!busy}
              onClick={() => void load()}
            >刷新</button>
          </div>

          {status && (
            <div className="space-y-1 text-[11px] text-nexus-muted">
              <div className="flex gap-2">
                <span className="text-nexus-muted/60 flex-shrink-0">计划任务</span>
                <span className="font-mono truncate" title={status.task_name}>{status.task_name || '（空）'}</span>
              </div>
              <div className="flex gap-2">
                <span className="text-nexus-muted/60 flex-shrink-0">登录类型</span>
                <span className={status.task_logon_type === 'S4U' ? 'text-nexus-text' : 'text-nexus-warning'}>
                  {status.task_logon_type || '—'}
                  {status.installed && status.task_logon_type !== 'S4U' && '（本版本只支持 S4U）'}
                </span>
              </div>
              <div className="flex gap-2">
                <span className="text-nexus-muted/60 flex-shrink-0">启动器文件</span>
                <span className={status.runner_installed ? 'text-nexus-text' : 'text-nexus-warning'}>
                  {status.runner_installed ? '已安装' : '缺失'}
                </span>
              </div>
              {status.runner_path && (
                <div className="flex gap-2">
                  <span className="text-nexus-muted/60 flex-shrink-0">安装路径</span>
                  <span className="font-mono break-all" title={status.runner_path}>{status.runner_path}</span>
                </div>
              )}
              <div className="flex gap-2">
                <span className="text-nexus-muted/60 flex-shrink-0">运行实例</span>
                <span>
                  {status.runner_reachable
                    ? `PID ${status.runner_pid ?? '?'} · 心跳 ${status.heartbeat_at || '—'}`
                    : '未运行 / 无响应'}
                </span>
              </div>
              <div className="flex gap-2">
                <span className="text-nexus-muted/60 flex-shrink-0">服务数</span>
                <span>{status.boot_service_count} 个服务已设置开机启动</span>
              </div>
              {status.message && (
                <div className={`leading-relaxed ${status.runner_reachable ? 'text-nexus-muted/70' : 'text-nexus-warning'}`}>
                  {status.message}
                </div>
              )}
              {status.installed && !status.task_path_ok && (
                <div className="text-nexus-warning leading-relaxed">
                  任务指向的程序或 manifest 与当前 Nexus 不一致，请点「修复 / 重新安装」。
                </div>
              )}
            </div>
          )}

          {error && (
            <div className="text-[11px] text-nexus-error leading-relaxed whitespace-pre-wrap break-all">
              {error}
            </div>
          )}

          <div className="flex flex-wrap items-center gap-1.5 pt-1">
            {!status?.installed ? (
              <button
                className="px-2 py-1 text-[11px] text-nexus-accent border border-nexus-accent/40 rounded hover:bg-nexus-accent/10 disabled:opacity-40"
                disabled={!!busy}
                onClick={() => void run('安装开机启动器', bootApi.install, '开机启动器已安装')}
              >{busy === '安装开机启动器' ? '等待管理员授权…' : '安装开机启动器（需要管理员）'}</button>
            ) : (
              <>
                <button
                  className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
                  disabled={!!busy}
                  onClick={() => void run('修复 / 重新安装', bootApi.install, '开机启动器已修复')}
                >{busy === '修复 / 重新安装' ? '等待管理员授权…' : '修复 / 重新安装'}</button>
                <button
                  className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
                  disabled={!!busy}
                  onClick={() => void run('测试启动', bootApi.start, '已请求启动开机启动器')}
                >{busy === '测试启动' ? '启动中…' : '测试启动'}</button>
                <button
                  className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-warning hover:text-nexus-warning disabled:opacity-40"
                  disabled={!!busy || !status.runner_reachable}
                  onClick={() => void run('停止启动器', bootApi.stop, '已请求停止开机启动器')}
                >{busy === '停止启动器' ? '停止中…' : '停止启动器'}</button>
                <button
                  className="px-2 py-1 text-[11px] text-nexus-error border border-nexus-error/30 rounded hover:bg-nexus-error/10 disabled:opacity-40"
                  disabled={!!busy}
                  onClick={() => void run('卸载开机启动器', bootApi.uninstall, '开机启动器已卸载')}
                >{busy === '卸载开机启动器' ? '卸载中…' : '卸载'}</button>
              </>
            )}
          </div>
        </div>

        <p className="mt-2.5 text-[11px] text-nexus-muted/80 leading-relaxed">
          开机启动器使用 Windows 计划任务的 <span className="font-mono">S4U</span> 登录：
          <strong className="text-nexus-text">不保存 Windows 密码</strong>，因此 PIN / 微软账户也能用；
          代价是它只能访问本地资源（网络共享、DPAPI 加密文件可能读不到）。
          它由当前用户运行，<strong className="text-nexus-text">不会</strong>以 SYSTEM 身份执行你的服务。
          首次安装/修复/卸载需要一次管理员授权；之后每个服务的开关不再需要提权。
        </p>
        <p className="mt-1.5 text-[11px] text-nexus-muted/60 leading-relaxed">
          注意：Windows 的“快速启动”可能让“关机→开机”走混合恢复而不是真正启动；若发现登录前没有启动，
          请先点「刷新」看运行实例，再用「测试启动」验证，必要时重启一次或关闭快速启动。
          Docker Desktop、需要桌面或网络共享的服务不支持登录前启动。
        </p>
      </section>
    </div>
  );
}

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider mb-2">
      {children}
    </div>
  );
}
