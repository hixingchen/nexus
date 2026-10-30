import { useCallback, useEffect, useRef, useState } from 'react';
import { bootApi, type BootRunnerStatus, type BootServiceStatus } from '../../services/boot';
import { processApi } from '../../services/service';
import { Modal } from '../ui/Modal';
import { LogViewer } from '../terminal/LogViewer';
import { notify } from '../../utils/notify';
import { reportError, toMessage } from '../../utils/error';

type Mode = 's4u' | 'password';
type ServiceOp = 'start' | 'stop' | 'restart';

const MODES: { key: Mode; label: string; short: string }[] = [
  { key: 's4u', label: 'S4U（不保存密码）', short: 'S4U' },
  { key: 'password', label: '保存密码', short: '保存密码' },
];

/** 状态轮询间隔：runner 可能被系统/计划任务/其他窗口停止，界面要能自己追上 */
const POLL_MS = 5000;

/**
 * 设置 > 服务开机启动。
 *
 * 两套**相互独立**的启动器：
 * - S4U：不保存密码，PIN/微软账户可用，但只能访问本地资源；
 * - 保存密码：Windows 原生凭据框输入密码，密码只交给计划任务，Nexus 不碰。
 *
 * 每个服务在服务编辑面板里选自己的模式；两套 runner/task/manifest 同时存在，
 * 项目 A 可以 S4U、项目 B 可以保存密码，互不影响。
 *
 * 本页同时展示**逐服务状态**：服务是否在跑由后端用 pid + 进程启动时刻独立判断，
 * 启动器失联时也能看到由它拉起、仍然存活的实例并直接操作。
 */
export function ServiceBootSection() {
  const [statuses, setStatuses] = useState<BootRunnerStatus[]>([]);
  const [services, setServices] = useState<BootServiceStatus[]>([]);
  /** 读取状态的错误（每次轮询成功后自动清掉） */
  const [error, setError] = useState('');
  /** 动作失败的错误（保留到下一次动作，不被 5 秒轮询清掉——例如“启动已阻止”的原因） */
  const [actionError, setActionError] = useState('');
  const [busy, setBusy] = useState('');
  const [busyService, setBusyService] = useState('');
  /** 「停止启动器」会停掉该模式全部服务，两步确认 */
  const [confirmStop, setConfirmStop] = useState<Mode | null>(null);
  /** 卸载会停掉全部服务并删除任务，同样两步确认 */
  const [confirmUninstall, setConfirmUninstall] = useState<Mode | null>(null);
  const [logTarget, setLogTarget] = useState<BootServiceStatus | null>(null);
  /**
   * 状态代际号：动作后的刷新会 +1，在途的 5 秒轮询响应据此作废。
   *
   * 没有它会出现用户报过的形态：点「停止启动器」后动作已返回“已停止”，但一拍更早
   * 发出的轮询响应随后到达，又把卡片改回“运行中”，直到下一拍才纠正。
   */
  const loadSeqRef = useRef(0);

  const load = useCallback(async (afterAction = false) => {
    const seq = afterAction ? ++loadSeqRef.current : loadSeqRef.current;
    const [statusRes, serviceRes] = await Promise.allSettled([
      bootApi.getStatus(),
      bootApi.getServices(),
    ]);
    // 期间有动作完成的刷新 → 这次响应已过期，丢弃
    if (seq !== loadSeqRef.current) return;
    if (statusRes.status === 'fulfilled') setStatuses(statusRes.value);
    if (serviceRes.status === 'fulfilled') setServices(serviceRes.value);
    // 两边独立落地：一个失败不该把另一份已经拿到的数据清空
    const failure = statusRes.status === 'rejected' ? statusRes.reason
      : serviceRes.status === 'rejected' ? serviceRes.reason : null;
    if (failure) {
      setError(toMessage(failure));
      reportError('读取服务开机启动状态失败', failure, { silent: true });
    } else {
      setError('');
    }
  }, []);

  useEffect(() => {
    void load();
    const timer = setInterval(() => { void load(); }, POLL_MS);
    return () => clearInterval(timer);
  }, [load]);

  const run = async (label: string, fn: () => Promise<unknown>, okText: string) => {
    setBusy(label);
    setActionError('');
    try {
      await fn();
      await load(true);
      notify({ title: okText });
    } catch (e: unknown) {
      await load(true);
      setActionError(toMessage(e));
      reportError(`${label}失败`, e, { silent: true });
    } finally {
      setBusy('');
    }
  };

  const runService = async (svc: BootServiceStatus, op: ServiceOp) => {
    const key = `${op}:${svc.service_id}`;
    const opLabel = op === 'start' ? '启动' : op === 'stop' ? '停止' : '重启';
    // stop 是兜底路径（runner 失联时按 PID 结束）可能耗时；restart 要等旧进程退出
    setBusyService(key);
    setActionError('');
    try {
      if (op === 'start') await processApi.start(svc.service_id);
      else if (op === 'stop') await processApi.stop(svc.service_id);
      else await processApi.restart(svc.service_id);
      await load(true);
      notify({ title: `已${opLabel}「${svc.name}」` });
    } catch (e: unknown) {
      // 动作可能已部分生效（例如 restart 停了旧进程但没能启动新进程），先刷新再展示原因
      await load(true);
      setActionError(toMessage(e));
      reportError(`${opLabel}服务「${svc.name}」失败`, e, { silent: true });
    } finally {
      setBusyService('');
    }
  };

  const stateChip = (status: BootRunnerStatus | undefined) => {
    if (!status) return { text: '读取中…', cls: 'bg-nexus-hover/40 text-nexus-muted' };
    switch (status.state) {
      case 'running':
        return { text: '运行中', cls: 'bg-nexus-success/15 text-nexus-success' };
      case 'stopped':
        return { text: '已停止', cls: 'bg-nexus-hover/40 text-nexus-muted' };
      case 'installed_stopped':
        return { text: '已安装 · 未运行', cls: 'bg-nexus-hover/40 text-nexus-muted' };
      case 'stale':
        return { text: '运行异常', cls: 'bg-nexus-warning/15 text-nexus-warning' };
      // 旧后端把“进程已退出”也叫 unreachable；按同一个语义显示
      case 'exited':
      case 'unreachable':
        return { text: '已退出', cls: 'bg-nexus-warning/15 text-nexus-warning' };
      default:
        return { text: '未安装', cls: 'bg-nexus-hover/40 text-nexus-muted' };
    }
  };

  const hasLegacy = services.some(s => s.mode === 'legacy');

  return (
    <div className="space-y-5">
      <section>
        <div className="flex items-center gap-2 mb-2">
          <SectionTitle>服务开机启动（登录前）</SectionTitle>
          <button
            className="ml-auto px-2 py-1 text-[11px] text-nexus-muted border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
            disabled={!!busy || !!busyService}
            onClick={() => void load()}
          >刷新</button>
        </div>
        <p className="text-[11px] text-nexus-muted/80 leading-relaxed mb-2">
          两套启动器相互独立：服务在编辑面板里选择用哪一套，项目 A 和项目 B 可以分别用 S4U / 保存密码。
          安装任一模式会自动停止并删除旧版的单任务，避免开机时两套任务同时拉起服务。
        </p>

        {(actionError || error) && (
          <div className="text-[11px] text-nexus-error leading-relaxed whitespace-pre-wrap break-all mb-2">
            {actionError || error}
          </div>
        )}

        {hasLegacy && (
          <div className="text-[11px] text-nexus-warning leading-relaxed mb-2">
            检测到旧版单任务启动器的记录/实例：新版状态与日志按模式分目录存放。
            请在上方对应模式点「安装」完成迁移（会停止旧版任务），否则这里只能读旧状态、无法按新模式管理。
          </div>
        )}

        <div className="space-y-3">
          {MODES.map(({ key, label, short }) => {
            const status = statuses.find(s => s.mode === key);
            // 任务已卸载但进程还在：这比“未安装”更需要被看见（它会占住 runner 文件）
            const chip = status && !status.installed && status.runner_alive
              ? { text: '任务已卸载 · 进程残留', cls: 'bg-nexus-warning/15 text-nexus-warning' }
              : stateChip(status);
            const taskMode: Mode | null = !status?.task_logon_type
              ? null
              : status.task_logon_type.toUpperCase() === 'S4U' ? 's4u'
                : status.task_logon_type.toLowerCase() === 'password' ? 'password' : null;
            const runningMode: Mode | null = status?.running_mode === 's4u' ? 's4u'
              : status?.running_mode === 'password' ? 'password' : null;
            const installLabel = `安装 ${short}`;
            const repairLabel = `修复 ${short}`;
            const updatePwdLabel = '更新密码';
            const startLabel = `测试启动 ${short}`;
            const stopLabel = `停止 ${short}`;
            const uninstallLabel = `卸载 ${short}`;
            return (
              <div key={key} className="px-3 py-2.5 rounded-md border border-nexus-border bg-nexus-bg/30 space-y-2">
                <div className="flex items-center gap-2">
                  <span className="text-[12px] text-nexus-text font-medium flex-1">{label}</span>
                  <span className={`text-[10px] leading-[16px] px-1.5 rounded-full ${chip.cls}`}>
                    {chip.text}
                  </span>
                </div>

                {status && (
                  <div className="space-y-1 text-[11px] text-nexus-muted">
                    <div className="flex gap-2">
                      <span className="text-nexus-muted/60 flex-shrink-0">计划任务</span>
                      <span className="font-mono truncate" title={status.task_name}>{status.task_name || '（空）'}</span>
                    </div>
                    <div className="flex gap-2">
                      <span className="text-nexus-muted/60 flex-shrink-0">任务模式</span>
                      <span className={taskMode === key ? 'text-nexus-text' : 'text-nexus-warning'}>
                        {taskMode === 'password' ? '保存密码' : taskMode === 's4u' ? 'S4U' : '—'}
                        {status.installed && taskMode && taskMode !== key && '（与卡片不一致）'}
                      </span>
                    </div>
                    <div className="flex gap-2">
                      <span className="text-nexus-muted/60 flex-shrink-0">运行实例</span>
                      <span>
                        {status.runner_reachable
                          ? `${runningMode === 'password' ? '保存密码' : runningMode === 's4u' ? 'S4U' : '—'} · PID ${status.runner_pid ?? '?'}`
                          : status.runner_alive
                            ? `启动器进程仍在${status.runner_pid ? ` · PID ${status.runner_pid}` : ''}（心跳已过期）`
                            : status.state === 'stopped'
                              ? '已停止'
                              : status.state === 'exited'
                                ? '已退出'
                                : '未运行'}
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
                      <span>{status.boot_service_count} 个服务使用此模式</span>
                    </div>
                    {/* 残留实例用下面的独立警示条（可操作），这里不再重复同一句话 */}
                    {status.message && !(status.runner_alive && !status.installed) && (
                      <div className={`leading-relaxed ${status.runner_reachable ? 'text-nexus-muted/70' : 'text-nexus-warning'}`}>
                        {status.message}
                      </div>
                    )}
                    {status.installed && !status.task_path_ok && (
                      <div className="text-nexus-warning leading-relaxed">
                        任务指向的程序或 manifest 与当前配置不一致，请点「修复」。
                      </div>
                    )}
                  </div>
                )}

                {/* 任务已卸载但进程还在：这是“重新安装失败（文件被占用）”的直接原因 */}
                {status?.runner_alive && !status.installed && (
                  <div className="rounded-md border border-nexus-warning/30 bg-nexus-warning/10 px-2.5 py-1.5 text-[11px] text-nexus-warning leading-relaxed">
                    计划任务已卸载，但启动器进程仍在{status.runner_pid ? `（PID ${status.runner_pid}）` : ''}。
                    重新安装时会自动结束它；也可以点「结束残留启动器」先清理。
                  </div>
                )}

                <div className="flex flex-wrap items-center gap-1.5 pt-2 mt-1 border-t border-nexus-border/30">
                  {!status?.installed ? (
                    <>
                      <button
                        className={`h-7 px-2.5 text-[11px] border rounded-md disabled:opacity-40 ${
                          key === 'password'
                            ? 'text-nexus-warning border-nexus-warning/40 hover:bg-nexus-warning/10'
                            : 'text-nexus-accent border-nexus-accent/40 hover:bg-nexus-accent/10'
                        }`}
                        disabled={!!busy || !!busyService}
                        onClick={() => void run(installLabel, () => bootApi.install(key),
                          key === 'password' ? '已注册保存密码的开机启动任务' : '已安装 S4U 开机启动任务')}
                      >{busy === installLabel
                        ? (key === 'password' ? '等待授权/输入密码…' : '等待管理员授权…')
                        : (key === 'password' ? '安装（保存密码）' : '安装（S4U，无需密码）')}</button>
                      {status?.runner_alive && (
                        <button
                          className="h-7 px-2.5 text-[11px] text-nexus-error border border-nexus-error/40 rounded-md hover:bg-nexus-error/10 disabled:opacity-40"
                          disabled={!!busy || !!busyService}
                          onClick={() => void run(stopLabel, () => bootApi.stop(key), `已结束 ${short} 残留启动器`)}
                        >{busy === stopLabel ? '结束中…' : '结束残留启动器'}</button>
                      )}
                    </>
                  ) : (
                    <>
                      <button
                        className="h-7 px-2.5 text-[11px] text-nexus-text border border-nexus-border rounded-md hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
                        disabled={!!busy || !!busyService}
                        title="会先停止该模式启动器（其服务会退出），安装完成后若原本在运行会自动恢复"
                        onClick={() => void run(repairLabel, () => bootApi.install(key), `已修复 ${short} 启动器`)}
                      >{busy === repairLabel ? '停止旧实例并安装…' : '修复 / 重新安装'}</button>
                      {key === 'password' && (
                        <button
                          className="h-7 px-2.5 text-[11px] text-nexus-warning border border-nexus-warning/40 rounded-md hover:bg-nexus-warning/10 disabled:opacity-40"
                          disabled={!!busy || !!busyService}
                          onClick={() => void run(updatePwdLabel, () => bootApi.install('password'), '已更新保存密码')}
                        >{busy === updatePwdLabel ? '等待授权/输入密码…' : '更新密码'}</button>
                      )}
                      <button
                        className="h-7 px-2.5 text-[11px] text-nexus-text border border-nexus-border rounded-md hover:border-nexus-accent hover:text-nexus-accent disabled:opacity-40"
                        disabled={!!busy || !!busyService}
                        onClick={() => void run(startLabel, () => bootApi.start(key), `已请求启动 ${short} 启动器`)}
                      >{busy === startLabel ? '启动中…' : '测试启动'}</button>
                      <button
                        className="h-7 px-2.5 text-[11px] text-nexus-error border border-nexus-error/40 rounded-md hover:bg-nexus-error/10 disabled:opacity-40"
                        disabled={!!busy || !!busyService}
                        title="停止该模式启动器；它拉起的全部服务会退出"
                        onClick={() => { setConfirmUninstall(null); setConfirmStop(key); }}
                      >停止启动器</button>
                      <button
                        className="ml-auto h-7 px-1.5 text-[11px] text-nexus-muted hover:text-nexus-error hover:bg-nexus-error/10 rounded-md disabled:opacity-40"
                        disabled={!!busy || !!busyService}
                        onClick={() => { setConfirmStop(null); setConfirmUninstall(key); }}
                      >卸载</button>
                    </>
                  )}
                </div>

                {confirmStop === key && (
                  <div className="flex flex-wrap items-center gap-2 rounded-md border border-nexus-warning/30 bg-nexus-warning/10 px-2.5 py-1.5">
                    <span className="flex-1 min-w-[180px] text-[11px] text-nexus-warning leading-relaxed">
                      停止会结束该模式下的全部服务，之后可用「测试启动」重新拉起。
                    </span>
                    <button
                      className="h-7 px-2.5 text-[11px] text-nexus-error border border-nexus-error/40 rounded-md hover:bg-nexus-error/10 disabled:opacity-40"
                      disabled={!!busy || !!busyService}
                      onClick={() => {
                        setConfirmStop(null);
                        void run(stopLabel, () => bootApi.stop(key), `已停止 ${short} 启动器`);
                      }}
                    >{busy === stopLabel ? '停止中…' : '确认停止'}</button>
                    <button
                      className="h-7 px-2.5 text-[11px] text-nexus-muted border border-nexus-border rounded-md hover:text-nexus-text disabled:opacity-40"
                      disabled={!!busy || !!busyService}
                      onClick={() => setConfirmStop(null)}
                    >取消</button>
                  </div>
                )}

                {confirmUninstall === key && (
                  <div className="flex flex-wrap items-center gap-2 rounded-md border border-nexus-error/30 bg-nexus-error/10 px-2.5 py-1.5">
                    <span className="flex-1 min-w-[180px] text-[11px] text-nexus-error leading-relaxed">
                      卸载会停止该模式下的全部服务并删除计划任务；意外残留的实例也会被结束。
                    </span>
                    <button
                      className="h-7 px-2.5 text-[11px] text-nexus-error border border-nexus-error/40 rounded-md hover:bg-nexus-error/10 disabled:opacity-40"
                      disabled={!!busy || !!busyService}
                      onClick={() => {
                        setConfirmUninstall(null);
                        void run(uninstallLabel, () => bootApi.uninstall(key), `已卸载 ${short} 启动器`);
                      }}
                    >{busy === uninstallLabel ? '卸载中…' : '确认卸载'}</button>
                    <button
                      className="h-7 px-2.5 text-[11px] text-nexus-muted border border-nexus-border rounded-md hover:text-nexus-text disabled:opacity-40"
                      disabled={!!busy || !!busyService}
                      onClick={() => setConfirmUninstall(null)}
                    >取消</button>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </section>

      {/* ── 逐服务状态 ─────────────────────────────────────────
          与上面两张卡的分工：卡片看“启动器”，这里看“服务”。
          服务在不在跑由后端用 pid + 启动时刻判断，启动器失联时也能看到并操作。 */}
      <section>
        <div className="flex items-center gap-2 mb-2">
          <SectionTitle>服务状态</SectionTitle>
          <span className="ml-auto text-[10px] text-nexus-muted/50">每 5 秒自动刷新</span>
        </div>
        <p className="text-[11px] text-nexus-muted/80 leading-relaxed mb-2">
          只列出仍配置为开机启动的服务，以及仍由启动器管理的存活实例；「日志」读取对应模式目录下的日志文件。
          已删除服务的残留进程会标出来，只能停止或查看日志。
        </p>
        {services.length === 0 ? (
          <p className="text-[11px] text-nexus-muted/40 py-2">暂无开机启动服务</p>
        ) : (
          <div className="space-y-1.5">
            {services.map(svc => {
              const phase = servicePhase(svc);
              const deleted = !svc.service_exists || !svc.project_exists;
              const busyNow = busyService.endsWith(`:${svc.service_id}`);
              return (
                <div key={svc.service_id} className="rounded-md border border-nexus-border/50 bg-nexus-bg/30 px-3 py-2 space-y-1.5">
                  <div className="flex items-center gap-2 min-w-0">
                    <span className={`w-[6px] h-[6px] rounded-full flex-shrink-0 ${phase.dot}`} />
                    <span className="text-[12px] text-nexus-text font-medium truncate">{svc.name}</span>
                    <span className={`text-[9px] leading-[14px] px-1.5 rounded-full flex-shrink-0 ${
                      svc.mode === 'password'
                        ? 'bg-nexus-warning/12 text-nexus-warning'
                        : svc.mode === 'legacy'
                          ? 'bg-nexus-error/10 text-nexus-error'
                          : 'bg-nexus-accent/12 text-nexus-accent'
                    }`}>{modeLabel(svc.mode)}</span>
                    {!svc.service_exists ? (
                      <span className="text-[9px] leading-[14px] px-1.5 rounded-full flex-shrink-0 bg-nexus-error/10 text-nexus-error">
                        服务已删除
                      </span>
                    ) : !svc.project_exists ? (
                      <span className="text-[9px] leading-[14px] px-1.5 rounded-full flex-shrink-0 bg-nexus-error/10 text-nexus-error">
                        项目已删除
                      </span>
                    ) : !svc.configured ? (
                      <span className="text-[9px] leading-[14px] px-1.5 rounded-full flex-shrink-0 bg-nexus-hover/40 text-nexus-muted">
                        已关闭开机启动
                      </span>
                    ) : null}
                    {svc.configured && svc.service_exists && svc.project_exists && !svc.service_enabled && (
                      <span className="text-[9px] leading-[14px] px-1.5 rounded-full flex-shrink-0 bg-nexus-hover/40 text-nexus-muted">
                        服务已禁用 · 开机不启动
                      </span>
                    )}
                    <span className={`ml-auto text-[10px] flex-shrink-0 ${phase.textCls}`}>{phase.text}</span>
                  </div>
                  <div className="flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[10px] text-nexus-muted/60">
                    {svc.pid != null && <span className="font-mono">PID {svc.pid}</span>}
                    {svc.alive && !svc.runner_reachable && (
                      <span className="text-nexus-warning">
                        {svc.runner_alive ? '启动器无响应（进程仍在运行）' : '启动器已退出（进程仍在运行）'}
                      </span>
                    )}
                    {svc.ready_at && <span>就绪 {formatTime(svc.ready_at)}</span>}
                    {svc.error && <span className="text-nexus-warning break-all">{svc.error}</span>}
                  </div>
                  <div className="flex flex-wrap items-center gap-1.5">
                    <button
                      className="px-2 py-1 text-[11px] text-nexus-info border border-nexus-info/30 rounded hover:bg-nexus-info/10 disabled:opacity-40"
                      onClick={() => setLogTarget(svc)}
                    >日志</button>
                    {/* 已删除的服务没有可启动/重启的配置，只留停止（进程可能还在跑）与日志 */}
                    {!svc.alive && !deleted && (
                      <button
                        className="px-2 py-1 text-[11px] text-nexus-success border border-nexus-success/30 rounded hover:bg-nexus-success/10 disabled:opacity-40"
                        disabled={!!busy || !!busyService}
                        onClick={() => void runService(svc, 'start')}
                      >{busyNow && busyService.startsWith('start:') ? '启动中…' : '启动'}</button>
                    )}
                    {svc.alive && (
                      <>
                        {!deleted && (
                          <button
                            className="px-2 py-1 text-[11px] text-nexus-warning border border-nexus-warning/30 rounded hover:bg-nexus-warning/10 disabled:opacity-40"
                            disabled={!!busy || !!busyService}
                            onClick={() => void runService(svc, 'restart')}
                          >{busyNow && busyService.startsWith('restart:') ? '重启中…' : '重启'}</button>
                        )}
                        <button
                          className="px-2 py-1 text-[11px] text-nexus-error border border-nexus-error/30 rounded hover:bg-nexus-error/10 disabled:opacity-40"
                          disabled={!!busy || !!busyService}
                          onClick={() => void runService(svc, 'stop')}
                        >{busyNow && busyService.startsWith('stop:') ? '停止中…' : '停止'}</button>
                      </>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </section>

      <p className="text-[11px] text-nexus-muted/80 leading-relaxed">
        <strong className="text-nexus-text">S4U</strong> 不保存密码，PIN / 微软账户可用；只能访问本地资源。
        <strong className="text-nexus-text">保存密码</strong> 会弹出 Windows 原生凭据框，密码只交给计划任务；
        修改 Windows 密码后需点「更新密码」，PIN 不能用于密码模式。
        两套 runner 都以当前用户运行，不会以 SYSTEM 身份执行你的服务；切换服务的模式后，
        旧 runner 里正在跑的实例需要停止/重启后才会切换到新模式。
      </p>
      <p className="mt-1.5 text-[11px] text-nexus-muted/60 leading-relaxed">
        首次安装/修复/卸载/更新密码需要一次管理员授权。修复/重新安装会先停止该模式启动器
        （它拉起的服务会退出）再替换文件，原本在运行的会在安装后自动恢复；卸载会停止全部服务并删除计划任务，
        若进程仍残留，可用「结束残留启动器」清理。快速启动可能让“关机→开机”走混合恢复，
        登录前没启动时先用「测试启动」验证；Docker Desktop、需要桌面或网络共享的服务不支持登录前启动。
      </p>

      {/* 服务日志：从设置页直接打开，不依赖服务卡片是否显示“运行中” */}
      <Modal
        open={!!logTarget}
        title={logTarget ? `服务日志 · ${logTarget.name}` : '服务日志'}
        onClose={() => setLogTarget(null)}
        width="min(1100px, calc(100vw - 80px))"
      >
        <div style={{ height: 'min(70vh, 720px)' }}>
          {logTarget && (
            <LogViewer
              serviceKey={logTarget.service_id}
              serviceName={logTarget.name}
              fill
              // 进程活着时按 1 秒轮询文件；已退出则只取一次快照
              forcePoll={logTarget.alive}
              runningOverride={logTarget.alive}
            />
          )}
        </div>
      </Modal>
    </div>
  );
}

/** 服务状态 → 文案/颜色。phase 是 state.json 的原始值；alive 来自 pid 校验。 */
function servicePhase(s: BootServiceStatus): {
  text: string;
  textCls: string;
  dot: string;
} {
  if (!s.phase) {
    if (s.configured && !s.service_enabled) {
      return { text: '开机不会启动（服务已禁用）', textCls: 'text-nexus-muted/50', dot: 'bg-nexus-muted/40' };
    }
    return { text: '未由启动器管理', textCls: 'text-nexus-muted/50', dot: 'bg-nexus-muted/40' };
  }
  if (s.alive) {
    if (s.phase === 'starting') {
      return { text: '启动中', textCls: 'text-nexus-warning', dot: 'bg-nexus-warning' };
    }
    if (s.phase === 'failed') {
      return { text: '未就绪（进程仍在运行）', textCls: 'text-nexus-warning', dot: 'bg-nexus-warning' };
    }
    return { text: '运行中', textCls: 'text-nexus-success', dot: 'bg-nexus-success' };
  }
  switch (s.phase) {
    case 'failed':
      return {
        text: s.exit_code != null ? `启动失败（退出码 ${s.exit_code}）` : '启动失败',
        textCls: 'text-nexus-error',
        dot: 'bg-nexus-error',
      };
    case 'exited':
      return {
        text: s.exit_code != null && s.exit_code !== 0 ? `已退出（退出码 ${s.exit_code}）` : '已退出',
        textCls: s.exit_code != null && s.exit_code !== 0 ? 'text-nexus-error' : 'text-nexus-muted',
        dot: s.exit_code != null && s.exit_code !== 0 ? 'bg-nexus-error' : 'bg-nexus-muted/40',
      };
    case 'stopped':
      return { text: '已停止', textCls: 'text-nexus-muted/50', dot: 'bg-nexus-muted/40' };
    case 'skipped':
      return { text: '已跳过（前一个必需服务未就绪）', textCls: 'text-nexus-warning', dot: 'bg-nexus-warning' };
    case 'pending':
      return { text: '等待启动', textCls: 'text-nexus-muted/50', dot: 'bg-nexus-muted/40' };
    case 'ready':
      // phase 说 ready 但存活校验没通过：可能是 runner 退出/被强杀后状态没来得及更新，
      // 也可能是 GUI 所在登录会话无权查询 S4U 进程。**不能断言“进程已退出”**——
      // 用户实际看到过“显示已退出、进程还在跑”的误报（服务器场景）。
      return s.runner_reachable
        ? { text: '进程状态待确认（启动器仍在运行）', textCls: 'text-nexus-warning', dot: 'bg-nexus-warning' }
        : { text: '无法确认进程状态（启动器已退出）', textCls: 'text-nexus-warning', dot: 'bg-nexus-warning' };
    default:
      return { text: s.phase, textCls: 'text-nexus-muted/50', dot: 'bg-nexus-muted/40' };
  }
}

function modeLabel(mode: string) {
  return mode === 'password' ? '保存密码' : mode === 'legacy' ? '旧版' : 'S4U';
}

/** ISO 时间 → 本地时间；解析不了就不显示（调用方用条件渲染兜住） */
function formatTime(ts: string): string {
  const d = new Date(ts);
  return isNaN(d.getTime()) ? '' : d.toLocaleString('zh-CN', { hour12: false });
}

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider">
      {children}
    </div>
  );
}
