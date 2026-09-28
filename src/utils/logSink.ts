/**
 * 前端日志回传的**端口**（NEW-28）：让 `utils/error.ts` 能把错误送回后端，
 * 而不必 import `services/system.ts`。
 *
 * 为什么需要反转（与 `utils/notify.ts` 同一手法，ARCH-17）：`services/system.ts`
 * import 了 `reportError`，而 `reportError` 又要调用回传实现——两边直接 import 就是
 * 一个环（构建期可能报警告，而本仓的 `vite build` 是"警告即失败"）。这里只留一个注册点：
 * 实现由 `services/system.ts` 在模块加载时注册进来，`utils/` 只认这个端口。
 *
 * 未注册时**丢弃这一条日志**、不抛错也不提示：唯一的未注册场景是 node 测试
 * （只 import store/utils，不 import services），而为了"没接上日志"去干扰用户
 * 是本末倒置——回传日志是顺带留痕，不是功能。
 */

/** 与后端 `logger::normalize_level` 的白名单一致（未知级别在后端降到 info） */
export type FrontendLogLevel = 'error' | 'warn' | 'info' | 'debug';

type LogSink = (level: FrontendLogLevel, message: string) => void;

let sink: LogSink | null = null;

/** 由服务层调用（`services/system.ts` 注册自己）。传 null 可注销（测试用） */
export function setFrontendLogSink(s: LogSink | null): void {
  sink = s;
}

/** 把一条日志送回后端（长度上限 / 换行过滤 / 去重限流都在后端做，见 `commands/app.rs`） */
export function logToBackend(level: FrontendLogLevel, message: string): void {
  sink?.(level, message);
}
