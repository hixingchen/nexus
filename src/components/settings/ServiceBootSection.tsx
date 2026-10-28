import { useCallback, useEffect, useState } from 'react';
import { bootApi, type BootRunnerStatus } from '../../services/boot';
import { notify } from '../../utils/notify';
import { reportError, toMessage } from '../../utils/error';

/**
 * 设置 > 服务开机启动。
 *
 * 这个分类只管**启动器本身**（计划任务 + runner 的安装/模式/状态/启停）；
 * 具体哪个服务要开机启动，在服务编辑面板里逐个勾选。
 *
 * 两种凭据模式：
 * - `S4U`：不保存密码，PIN/微软账户可用，但只能访问本地资源；
 * - `Password`：登录时用 Windows 原生凭据框输入密码，密码只交给计划任务，
 *   Nexus 数据库/日志/命令行都不碰；改 Windows 密码后需要重新输入。
 *
 * 状态一律以**回读计划任务 + runner 心跳**为准，不拿本地缓存猜（铁律 21）。
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

  const install = (mode: 's4u' | 'password', label: string) => {
    void run(label, () => bootApi.install(mode), mode === 'password'
      ? '已注册保存密码的开机启动任务'
      : '已安装 S4U 开机启动任务');
  };

  const taskMode: 's4u' | 'password' | null = !status?.task_logon_type
    ? null
    : status.task_logon_type.toLowerCase() === 'password'
      ? 'password'
      : status.task_logon_type.toUpperCase() === 'S4U' ? 's4u' : null;
  const modeText = (mode: 's4u' | 'password' | null) => (
    mode === 'password' ? '保存密码' : mode === 's4u' ? 'S4U（不保存密码）' : '—'
  );
  const modeShort = (mode: 's4u' | 'password' | null) => (
    mode === 'password' ? '保存密码' : mode === 's4u' ? 'S4U' : '—'
  );

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
                <span className="text-nexus-muted/60 flex-shrink-0">任务模式</span>
                <span className={taskMode ? 'text-nexus-text' : 'text-nexus-warning'}>
                  {modeText(taskMode)}
                  {status.installed && status.task_logon_type && !taskMode && '（无法识别的登录类型）'}
                </span>
              </div>
              <div className="flex gap-2">
                <span className="text-nexus-muted/60 flex-shrink-0">运行实例</span>
                <span>
                  {status.runner_reachable
                    ? `${modeText(status.running_mode === 'password' ? 'password' : status.running_mode === 's4u' ? 's4u' : null)} · PID ${status.runner_pid ?? '?'}`
                    : '未运行 / 无响应'}
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
              <>
                <button
                  className="px-2 py-1 text-[11px] text-nexus-accent border border-nexus-accent/40 rounded hover:bg-nexus-accent/10 disabled:opacity-40"
                  disabled={!!busy}
                  onClick={() => install('s4u', '安装（S4U）')}
                >{busy === '安装（S4U）' ? '等待管理员授权…' : '安装（S4U，无需密码）'}</button>
                <button
                  className="px-2 py-1 text-[11px] text-nexus-warning border border-nexus-warning/40 rounded hover:bg-nexus-warning/10 disabled:opacity-40"
                  disabled={!!busy}
                  onClick={() => install('password', '安装（保存密码）')}
                >{busy === '安装（保存密码）' ? '等待授权/输入密码…' : '安装（保存密码）'}</button>
              </>
            ) : (
              <>
                <button
                  className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
                  disabled={!!busy}
                  onClick={() => install(taskMode ?? 's4u', '修复 / 重新安装')}
                >{busy === '修复 / 重新安装' ? '等待管理员授权…' : `修复 / 重新安装（${modeShort(taskMode)}）`}</button>
                {taskMode === 's4u' ? (
                  <button
                    className="px-2 py-1 text-[11px] text-nexus-warning border border-nexus-warning/40 rounded hover:bg-nexus-warning/10 disabled:opacity-40"
                    disabled={!!busy}
                    onClick={() => install('password', '切换为保存密码')}
                  >{busy === '切换为保存密码' ? '等待授权/输入密码…' : '切换为保存密码'}</button>
                ) : taskMode === 'password' ? (
                  <>
                    <button
                      className="px-2 py-1 text-[11px] text-nexus-warning border border-nexus-warning/40 rounded hover:bg-nexus-warning/10 disabled:opacity-40"
                      disabled={!!busy}
                      onClick={() => install('password', '更新密码')}
                    >{busy === '更新密码' ? '等待授权/输入密码…' : '更新密码'}</button>
                    <button
                      className="px-2 py-1 text-[11px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
                      disabled={!!busy}
                      onClick={() => install('s4u', '切换为 S4U')}
                    >{busy === '切换为 S4U' ? '等待管理员授权…' : '切换为 S4U（不保存密码）'}</button>
                  </>
                ) : null}
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
          <strong className="text-nexus-text">S4U（默认）</strong>：不保存密码，PIN / 微软账户也能用；
          只能访问本地资源（网络共享、DPAPI 加密文件可能读不到）。
          <strong className="text-nexus-text">保存密码</strong>：会弹出 Windows 原生凭据框，
          密码只交给 Windows 计划任务保存，Nexus 数据库/日志/命令行都不碰。
          修改 Windows 密码后，密码模式需要点「更新密码」；PIN 不能用于密码模式。
          切换模式会改写同一个计划任务，当前运行实例要停止/重启启动器后才使用新凭据。
        </p>
        <p className="mt-1.5 text-[11px] text-nexus-muted/60 leading-relaxed">
          启动器以当前用户运行，<strong className="text-nexus-text">不会</strong>以 SYSTEM 身份执行你的服务；
          首次安装/修复/卸载/切换模式需要一次管理员授权。快速启动可能让“关机→开机”走混合恢复，
          登录前没启动时先用「测试启动」验证；Docker Desktop、需要桌面或网络共享的服务不支持登录前启动。
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
