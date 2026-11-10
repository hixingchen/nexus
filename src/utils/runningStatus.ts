import type { FailedService, PortConflict, RunningService } from '../services/service';

/** 端口数组逐项比较（后端保证升序去重，这里仍按内容比，不依赖顺序以外的约定） */
function samePorts(a: number[] | undefined, b: number[] | undefined): boolean {
  const pa = a ?? [];
  const pb = b ?? [];
  return pa.length === pb.length && pa.every((port, i) => port === pb[i]);
}

/**
 * 端口冲突信息逐字段比较。
 *
 * 为什么必须比：失败项的 `port_conflict` 可能**晚一拍**才由后端从日志里补出来
 * （tick 时 stderr 还没刷完，下一次 get_running 才识别到）。漏比它就会被判成
 * "失败项没变化"而丢弃这次响应，失败卡片上的占用者信息永远不出现。
 */
function samePortConflict(a?: PortConflict, b?: PortConflict): boolean {
  if (!a && !b) return true;
  if (!a || !b) return false;
  if (a.port !== b.port || a.occupants.length !== b.occupants.length) return false;
  return a.occupants.every((o, i) => {
    const y = b.occupants[i];
    return y && o.pid === y.pid && o.name === y.name && o.path === y.path && o.is_self === y.is_self;
  });
}

/**
 * 比对两组运行/失败列表是否等价（逐字段、按下标）。
 *
 * 后端每次返回新数组；内容未变时照样 `set` 等于换引用，会把所有订阅方（项目行 /
 * 服务面板 / 展开的目录树）全部重渲染。抽成纯函数是为了能直接被 Node 测试器覆盖，
 * 不再依赖 store 里仅供测试的 `setRunning` 入口。
 */
export function sameRunningStatus(
  a: RunningService[],
  b: RunningService[],
  af: FailedService[],
  bf: FailedService[],
): boolean {
  return a.length === b.length
    && af.length === bf.length
    && a.every((x, i) => {
      const y = b[i];
      // **三个字段都要比**（CQ-24）：`followed_log` 是**异步出现**的——后端每次
      // `get_running` 都驱动两阶段采样，服务启动时要等日志文件“在长”才认领，于是
      // “成员与顺序都没变、只是它从 null 变成了路径”是常见形态。漏比它就会被判成
      // “没变化”而早退，`followedLogOf` 恒为 null，「取消跟随日志（xxx.log）」入口
      // 永远不出现。failed 分支本来就是逐字段全比的，这里与它对齐。
      return y
        && x.service_id === y.service_id
        && x.project_id === y.project_id
        && x.source === y.source
        && x.phase === y.phase
        && (x.health_note ?? null) === (y.health_note ?? null)
        && samePorts(x.listening_ports, y.listening_ports)
        && (x.followed_log ?? null) === (y.followed_log ?? null);
    })
    && af.every((x, i) => {
      const y = bf[i];
      return y && x.service_id === y.service_id && x.exit_code === y.exit_code
        && x.timestamp === y.timestamp && x.source === y.source
        && samePortConflict(x.port_conflict, y.port_conflict);
    });
}
