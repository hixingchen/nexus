import { useEffect, useRef, useState, type FormEvent } from 'react';
import { Modal } from '../ui/Modal';
import { toMessage } from '../../utils/message';

interface NameEntryModalProps {
  open: boolean;
  title: string;
  submitLabel: string;
  initialValue: string;
  /** 文件重命名时只选中主名（不含扩展名） */
  selectStem?: boolean;
  onClose: () => void;
  /** 抛错 → 错误显示在弹窗内并保持打开；resolve = 成功（由调用方关闭） */
  onSubmit: (name: string) => Promise<void>;
}

/**
 * 新建文件/文件夹与重命名共用的名字输入弹窗。
 *
 * 为什么用弹窗而不是行内编辑（第一版）：`Entry` 是 memo 组件，行内编辑要给每行加
 * 输入状态，容易让整棵树一起重渲染；弹窗实现简单，错误也能原地展示后重试。
 *
 * 错误**留在弹窗里**而不是 toast：后端拒绝同名/非法名称后，用户要改的是刚输入的那个
 * 名字；弹窗一关就得从头再来一次。
 */
export function NameEntryModal({
  open,
  title,
  submitLabel,
  initialValue,
  selectStem,
  onClose,
  onSubmit,
}: NameEntryModalProps) {
  const [value, setValue] = useState(initialValue);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (!open) return;
    setValue(initialValue);
    setError(null);
    setSubmitting(false);
    // Modal 挂载时会聚焦第一个输入框；这里再补一次选中范围（文件重命名不含扩展名）
    const frame = requestAnimationFrame(() => {
      const input = inputRef.current;
      if (!input) return;
      input.focus();
      const dot = initialValue.lastIndexOf('.');
      if (selectStem && dot > 0) input.setSelectionRange(0, dot);
      else input.select();
    });
    return () => cancelAnimationFrame(frame);
  }, [open, initialValue, selectStem]);

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (submitting) return;
    const name = value.trim();
    if (!name) {
      setError('名称不能为空');
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      await onSubmit(name);
      // 成功时由调用方关闭弹窗（它还要刷新树/迁移标签）
    } catch (err) {
      setError(toMessage(err));
      setSubmitting(false);
    }
  };

  return (
    <Modal
      open={open}
      title={title}
      width="380px"
      onClose={() => { if (!submitting) onClose(); }}
    >
      <form className="space-y-3" onSubmit={handleSubmit}>
        <label className="block">
          <span className="block text-[12px] text-nexus-muted mb-1.5">名称</span>
          <input
            ref={inputRef}
            value={value}
            disabled={submitting}
            onChange={e => { setValue(e.target.value); setError(null); }}
            className="w-full bg-nexus-bg border border-nexus-border rounded px-2.5 py-1.5 text-[13px] text-nexus-text outline-none focus:border-nexus-accent disabled:opacity-50"
          />
        </label>
        {error && <p className="text-[12px] text-nexus-error break-words">{error}</p>}
        <div className="flex items-center justify-end gap-2 pt-1">
          <button
            type="button"
            disabled={submitting}
            onClick={onClose}
            className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50 disabled:opacity-40"
          >取消</button>
          <button
            type="submit"
            disabled={submitting || !value.trim()}
            className="px-5 py-1.5 text-[13px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover disabled:opacity-40"
          >{submitting ? '处理中…' : submitLabel}</button>
        </div>
      </form>
    </Modal>
  );
}
