// ── 日志级别分类与过滤 ─────────────────────────────────────────
//
// 目标：从进程输出里认出「错误 / 警告」，并让堆栈续行跟着所属错误一起保留。
// 纯函数、无依赖，Node 内置测试器可直接运行。
//
// 分类是**启发式**的：日志格式由各服务自己决定，这里只覆盖常见写法
// （Spring Boot / Node / Python / Tomcat / Maven）。宁可漏判，也不要把普通
// 正文里的 "error"（URL、JSON 消息字段、更长的单词）误判成错误。

import type { ServiceLogLine } from '../services/logService';

export type LogSeverity = 'error' | 'warn' | 'info' | 'debug' | 'unknown';
/** 头部过滤模式：全部 / 只看错误 / 警告及以上 */
export type LogSeverityFilter = 'all' | 'error' | 'warn';

export interface LogClassification {
  severity: LogSeverity;
  /** 堆栈/多行消息的续行：级别继承上一段，过滤时随它一起保留 */
  continuation: boolean;
}

export interface LogFilterResult {
  rows: ServiceLogLine[];
  /** 明确判为错误的主行（不含续行） */
  errorCount: number;
  /** 明确判为警告的主行（不含续行） */
  warnCount: number;
  /** 随主行保留的堆栈续行 */
  continuationCount: number;
}

// eslint-disable-next-line no-control-regex -- ANSI 转义序列的起始字节，不是笔误
const ANSI_RE = /\x1b\[[0-9;]*m/g;

const ERROR_LEVELS = new Set(['FATAL', 'ERROR', 'CRITICAL', 'PANIC', 'EXCEPTION', 'SEVERE', 'FAILED', 'ERR']);
const WARN_LEVELS = new Set(['WARN', 'WARNING']);
const INFO_LEVELS = new Set(['INFO', 'INFORMATION']);
const DEBUG_LEVELS = new Set(['DEBUG', 'TRACE', 'VERBOSE']);
const LEVELS = [...ERROR_LEVELS, ...WARN_LEVELS, ...INFO_LEVELS, ...DEBUG_LEVELS];

const LEVEL_TOKEN_RE = new RegExp(`\\b(${LEVELS.join('|')})\\b`, 'gi');
const LEVEL_FIELD_RE = new RegExp(
  `["']?level["']?\\s*[=:]\\s*["']?(${LEVELS.join('|')})\\b`,
  'i',
);
const BRACKET_LEVEL_RE = new RegExp(`\\[(${LEVELS.join('|')})\\]`, 'i');

/** 常见日志位置上的裸级别（时间戳/线程名之后），限制在前缀区避免把正文里的词当级别 */
const BARE_LEVEL_PREFIX = 64;

/** 无缩进也能认出的堆栈标记（Java/Node 的 `at ...` 等） */
const STACK_MARKER_RE = /^(?:at\s|Caused by:|Suppressed:|\.\.\.\s*\d+\s+more\b|node:internal[\\/])/;

/** Python / Node 的异常头：ValueError: ... / TypeError: ... */
const EXCEPTION_HEAD_RE = /^\s*(?:[A-Za-z_$][\w$]*\.)*(?:[A-Za-z_$][\w$]*Error|[A-Za-z_$][\w$]*Exception)\s*:/;

function severityOfLevel(level: string): LogSeverity {
  const upper = level.toUpperCase();
  if (ERROR_LEVELS.has(upper)) return 'error';
  if (WARN_LEVELS.has(upper)) return 'warn';
  if (INFO_LEVELS.has(upper)) return 'info';
  if (DEBUG_LEVELS.has(upper)) return 'debug';
  return 'unknown';
}

/** 级别词是否处在一个像日志级别的位置（边界 + 不在 JSON 消息字符串里） */
function isLevelPosition(text: string, index: number, length: number): boolean {
  const before = index > 0 ? text[index - 1] : '';
  const after = text[index + length] ?? '';
  const leftOk = index === 0 || /[\s[(<|]/.test(before);
  const rightOk = index + length === text.length || /[\s:)\]|-]/.test(after);
  if (!leftOk || !rightOk) return false;
  // 结构化数据（JSON）里的普通消息字段不当作级别；明确的 level 字段在上面已单独识别
  const prefix = text.slice(0, index);
  if (/[{"]/.test(prefix)) return false;
  return true;
}

/** 从一行文本里识别明确写出的级别；识别不了返回 null */
function detectLevel(text: string): LogSeverity | null {
  const field = text.match(LEVEL_FIELD_RE);
  if (field) return severityOfLevel(field[1]);

  // `[ERROR]` 这类括号形式在任何位置都算明确级别（Maven / Tomcat）
  const bracket = text.match(BRACKET_LEVEL_RE);
  if (bracket) return severityOfLevel(bracket[1]);

  LEVEL_TOKEN_RE.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = LEVEL_TOKEN_RE.exec(text)) !== null) {
    if (match.index > BARE_LEVEL_PREFIX) break;
    if (!isLevelPosition(text, match.index, match[0].length)) continue;
    return severityOfLevel(match[0]);
  }

  if (/^\s*Traceback \(most recent call last\)/i.test(text)) return 'error';
  if (EXCEPTION_HEAD_RE.test(text)) return 'error';
  return null;
}

/**
 * system 流是 Nexus 自己写的生命周期标记，不是服务输出。
 * 失败服务的日志靠这些行解释原因，所以「退出码 / 进程已退出 / 启动失败」归错误。
 */
function detectSystemLevel(text: string): LogSeverity | null {
  if (/启动失败|进程已退出|退出码|启动命令失败|命令失败|跟随失败|无法唤醒/.test(text)) return 'error';
  if (/⚠|警告|未能纳入/.test(text)) return 'warn';
  return null;
}

/** 每行自身的级别（与前后文无关）；WeakMap 缓存：刷新帧生成新对象后自动失效 */
const ownLevelCache = new WeakMap<ServiceLogLine, LogSeverity>();

function ownLevel(line: ServiceLogLine): LogSeverity {
  const cached = ownLevelCache.get(line);
  if (cached) return cached;
  const text = line.text.replace(ANSI_RE, '');
  const level = (line.stream === 'system' ? detectSystemLevel(text) : null)
    ?? detectLevel(text)
    ?? 'unknown';
  ownLevelCache.set(line, level);
  return level;
}

/** 该行是否属于上一段错误的堆栈/多行消息 */
function isContinuationLine(text: string): boolean {
  const clean = text.replace(ANSI_RE, '');
  if (!clean.trim()) return false;
  // 缩进行承接上一段（堆栈帧、异常的多行消息）
  if (/^\s/.test(clean)) return true;
  return STACK_MARKER_RE.test(clean);
}

/**
 * 分类单行。`prev` 必须是**上一行的分类结果**（续行判定依赖它继承下来的级别）。
 */
export function classifyLogLine(
  line: ServiceLogLine,
  prev: LogClassification | null,
): LogClassification {
  const own = ownLevel(line);
  if (own !== 'unknown') return { severity: own, continuation: false };
  if (
    prev
    && (prev.severity === 'error' || prev.severity === 'warn')
    && isContinuationLine(line.text)
  ) {
    return { severity: prev.severity, continuation: true };
  }
  return { severity: 'unknown', continuation: false };
}

/** 顺序分类整批日志（续行会继承上一段的级别） */
export function classifyLogLines(lines: ServiceLogLine[]): LogClassification[] {
  const out: LogClassification[] = [];
  let prev: LogClassification | null = null;
  for (const line of lines) {
    prev = classifyLogLine(line, prev);
    out.push(prev);
  }
  return out;
}

/**
 * 按级别过滤。先分类再筛，保证堆栈续行不会从错误中间被截掉；
 * 返回的 rows 保持原顺序，可直接交给搜索过滤。
 */
export function filterLogsBySeverity(
  lines: ServiceLogLine[],
  mode: LogSeverityFilter,
): LogFilterResult {
  if (mode === 'all') {
    return { rows: lines, errorCount: 0, warnCount: 0, continuationCount: 0 };
  }

  const classes = classifyLogLines(lines);
  const rows: ServiceLogLine[] = [];
  let errorCount = 0;
  let warnCount = 0;
  let continuationCount = 0;

  for (let i = 0; i < lines.length; i++) {
    const c = classes[i];
    const keep = mode === 'error'
      ? c.severity === 'error'
      : c.severity === 'error' || c.severity === 'warn';
    if (!keep) continue;
    rows.push(lines[i]);
    if (c.continuation) continuationCount++;
    else if (c.severity === 'error') errorCount++;
    else if (c.severity === 'warn') warnCount++;
  }

  return { rows, errorCount, warnCount, continuationCount };
}
