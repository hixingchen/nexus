/**
 * 开机服务“就绪检查”在表单与后端 JSON 之间的转换。
 *
 * 后端只接受四种形状（见 `core::boot::BootCheck`）；这里做**纯函数**转换，
 * 非法输入返回错误而不是静默落库——否则用户会看到“保存成功”，下次开机才发现
 * 检查根本没生效。
 */

export type BootCheckKind = 'none' | 'delay' | 'tcp' | 'logContains';

export interface BootCheckForm {
  kind: BootCheckKind;
  /** delay=秒数；tcp=host:port；logContains=关键字；none=空 */
  value: string;
}

export const BOOT_CHECK_KIND_LABEL: Record<BootCheckKind, string> = {
  none: '进程存活即可',
  delay: '固定等待',
  tcp: 'TCP 端口可连接',
  logContains: '日志出现关键字',
};

export function parseBootCheck(raw: string | null | undefined): BootCheckForm {
  const text = (raw ?? '').trim();
  if (!text) return { kind: 'none', value: '' };
  try {
    const obj = JSON.parse(text) as Record<string, unknown>;
    switch (obj.type) {
      case 'delay':
        return { kind: 'delay', value: String(obj.seconds ?? '') };
      case 'tcp':
        return { kind: 'tcp', value: `${obj.host ?? ''}:${obj.port ?? ''}` };
      case 'logContains':
        return { kind: 'logContains', value: String(obj.pattern ?? '') };
      default:
        return { kind: 'none', value: '' };
    }
  } catch {
    return { kind: 'none', value: '' };
  }
}

export interface SerializeBootCheckResult {
  ok: boolean;
  json: string;
  error: string;
}

/** 表单 → 后端 JSON。`kind='none'` 返回空串（DB 的“未配置”口径）。 */
export function serializeBootCheck(kind: BootCheckKind, rawValue: string): SerializeBootCheckResult {
  const value = rawValue.trim();
  if (kind === 'none') return { ok: true, json: '', error: '' };

  if (kind === 'delay') {
    const seconds = Number(value);
    if (!Number.isInteger(seconds) || seconds < 0 || seconds > 3600) {
      return { ok: false, json: '', error: '固定等待必须是 0-3600 之间的整数秒' };
    }
    return { ok: true, json: JSON.stringify({ type: 'delay', seconds }), error: '' };
  }

  if (kind === 'tcp') {
    const idx = value.lastIndexOf(':');
    if (idx <= 0 || idx === value.length - 1) {
      return { ok: false, json: '', error: 'TCP 检查请填写 host:port，例如 127.0.0.1:3306' };
    }
    const host = value.slice(0, idx).trim();
    const port = Number(value.slice(idx + 1).trim());
    if (!host) return { ok: false, json: '', error: 'TCP 检查的 host 不能为空' };
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      return { ok: false, json: '', error: 'TCP 检查的端口必须在 1-65535 之间' };
    }
    return {
      ok: true,
      json: JSON.stringify({ type: 'tcp', host, port, timeoutSecs: 60 }),
      error: '',
    };
  }

  // logContains
  if (!value) return { ok: false, json: '', error: '日志关键字不能为空' };
  if (value.length > 512) return { ok: false, json: '', error: '日志关键字不能超过 512 个字符' };
  return {
    ok: true,
    json: JSON.stringify({ type: 'logContains', pattern: value, timeoutSecs: 120 }),
    error: '',
  };
}
