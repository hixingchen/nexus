// ── 服务失败的展示文案 ─────────────────────────────────────────
//
// 为什么单独成文件：同一句"失败长什么样"有三个展示位（服务卡片徽章、日志面板头部状态、
// 收起态圆点的 tooltip），各写一份必然漂移——而这三处说的必须是同一件事：用户在卡片上
// 看到"已失败 · 退出码 1"，点进日志面板却只看到"未运行"，正是这次要修掉的不一致。
// 纯函数、不碰 DOM，可直接被 Node 内置测试器跑。

import type { FailedService } from '../services/service';

/**
 * 状态徽章/标签文案。
 *
 * 为什么要把 spawn 失败单独写出来：它的 `exit_code` 是 null（进程根本没起来），
 * 与"跑起来之后退出"是两类问题——前者去查命令/工作目录，后者才需要在日志里找报错。
 */
export function failureLabel(failed: FailedService): string {
  return failed.exit_code === null ? '启动失败' : `已失败 · 退出码 ${failed.exit_code}`;
}

/** 悬停说明：哪一类失败 + 发生时刻（失败可能发生在几分钟前，"什么时候"是唯一线索） */
export function failureDetail(failed: FailedService): string {
  const clock = formatClock(failed.timestamp);
  const what = failed.exit_code === null
    ? '启动失败：进程没有起来'
    : `进程意外退出（退出码 ${failed.exit_code}）`;
  return clock ? `${what} · ${clock}` : what;
}

/** 本地 HH:MM:SS；空值 / 解析不了返回空串（与日志行时间戳同一口径，见 LogViewer 的 fmtTime） */
function formatClock(ts: string): string {
  if (!ts) return '';
  const d = new Date(ts);
  if (isNaN(d.getTime())) return '';
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}:${String(d.getSeconds()).padStart(2, '0')}`;
}
