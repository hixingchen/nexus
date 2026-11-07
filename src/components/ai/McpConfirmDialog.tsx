import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { aiService } from '../../services/aiService';
import { Modal } from '../ui/Modal';
import { reportError } from '../../utils/error';
import { showNotification } from '../ui/Toast';

/** 后端 `mcp-confirm-request` 事件载荷（与 commands/mcp_confirm.rs 的 ConfirmRequest 同名） */
interface ConfirmRequest {
  id: number;
  title: string;
  detail: string;
  timeout_secs: number;
}

/**
 * AI（MCP）高危操作的确认弹窗。
 *
 * 后端在 `nexus_invoke` / 语义工具执行任何"能跑代码、能改系统级设置"的命令前挂起，
 * 发这个事件并等用户点按钮；只有点「允许一次」才继续。**关闭弹窗（✕ / Esc / 点遮罩）
 * 一律按拒绝处理**——默认不放行。
 *
 * 用现有 `Modal` 还有一个关键作用：它会把 `uiStore.modalCount` 加一，AiPanel 据此把
 * **原生子 WebView** 移出屏幕——否则 dsh 页面会盖在这个确认框上面。
 */
export function McpConfirmDialog() {
  const [req, setReq] = useState<ConfirmRequest | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | null = null;
    void listen<ConfirmRequest>('mcp-confirm-request', (ev) => {
      if (!alive) return;
      setReq(ev.payload);
    })
      .then((fn) => {
        if (alive) unlisten = fn;
        else fn();
      })
      .catch((e) => reportError('订阅 AI 操作确认失败', e));
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  // 超时自动关闭：后端 120 秒后按"未允许"失败，界面不能一直挂着
  useEffect(() => {
    if (timerRef.current !== null) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    if (!req) return;
    timerRef.current = setTimeout(() => {
      timerRef.current = null;
      setReq(null);
      showNotification({
        variant: 'warning',
        title: 'AI 操作确认已超时',
        description: '未在时限内确认，操作没有执行。',
        duration: 5000,
      });
    }, req.timeout_secs * 1000);
    return () => {
      if (timerRef.current !== null) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, [req]);

  const respond = (approved: boolean) => {
    const id = req?.id;
    if (id == null) return;
    setReq(null);
    aiService.confirmMcpAction(id, approved).catch((e) => {
      // 常见于"用户点得比后端超时晚"：操作本就没执行，只需说明
      reportError('提交 AI 操作确认失败', e);
    });
  };

  return (
    <Modal
      open={req != null}
      title={req?.title ?? 'AI 操作确认'}
      onClose={() => respond(false)}
      width="560px"
    >
      <div className="space-y-3">
        <p className="text-[12px] text-nexus-text leading-relaxed whitespace-pre-wrap break-all max-h-[42vh] overflow-auto bg-nexus-editor border border-nexus-border rounded-md p-2.5">
          {req?.detail ?? ''}
        </p>
        <p className="text-[11.5px] text-nexus-muted leading-relaxed">
          这是 AI 通过 MCP 发起的操作，来自当前 AI 会话（它读取的项目文件 / 日志 / 网页内容都可能
          影响它的决定）。只有点「允许一次」才会执行；{req?.timeout_secs ?? 120} 秒内不操作将自动拒绝。
          可在「设置 &gt; AI 运行时」关闭此类确认（默认开启，AI 自身无法修改）。
        </p>
        <div className="flex justify-end gap-2">
          <button
            className="px-3 py-1 text-[12px] text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 transition-colors"
            onClick={() => respond(false)}
          >
            拒绝
          </button>
          <button
            className="px-3 py-1 text-[12px] bg-nexus-accent/15 text-nexus-accent rounded-md hover:bg-nexus-accent/25 transition-colors"
            onClick={() => respond(true)}
          >
            允许一次
          </button>
        </div>
      </div>
    </Modal>
  );
}
