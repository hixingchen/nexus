import { useState, useEffect, useLayoutEffect, useId, useRef } from 'react';
import { projectApi, type Project } from '../../services/service';
import { pickDirectory } from '../../services/system';
import { Modal } from '../ui/Modal';
import { showNotification } from '../ui/Toast';
import { reportError, toMessage } from '../../utils/error';
import { isSubmitEnter } from '../../utils/keyboard';
import {
  MODAL_DRAFT_KEYS, readModalDraft, useModalDraftStore, type ProjectFormDraft,
} from '../../stores/modalDraftStore';
import { shouldSyncProjectDraft } from '../../utils/projectEditDraft';

interface CreateModalProps {
  open: boolean;
  onClose: () => void;
  onCreated: (project: Project) => void;
}

/** 新建项目的草稿键（模块级常量：放进 effect 依赖里要求身份稳定） */
const CREATE_DRAFT_KEY = MODAL_DRAFT_KEYS.projectCreate;

export function CreateProjectModal({ open, onClose, onCreated }: CreateModalProps) {
  // 初值优先取草稿（UX-14）：本组件会随 ProjectList 在窄轨收起时整块卸载，
  // 光靠 useState 活不过那一次卸载
  const [name, setName] = useState(() => readModalDraft<ProjectFormDraft>(CREATE_DRAFT_KEY)?.name ?? '');
  const [path, setPath] = useState(() => readModalDraft<ProjectFormDraft>(CREATE_DRAFT_KEY)?.path ?? '');
  const [saving, setSaving] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);
  /** 表单字段 id 前缀（UX-11）：label 的 htmlFor 要指向真实存在的 id，用 useId 避免多处手写撞车 */
  const uid = useId();

  // 任何输入都同步进草稿：关闭（遮罩 / Esc / ✕）只是离开，内容留到下次打开。
  // 空表单不留草稿——没有内容就没有"丢失"可言（改回空串会自动清掉）
  useEffect(() => {
    useModalDraftStore.getState().setDraft(CREATE_DRAFT_KEY, (name || path) ? { name, path } : null);
  }, [name, path]);

  useEffect(() => {
    if (open) {
      // 这里**不再清空表单**：那次清空正是"填一半点遮罩就全丢"的实现（UX-14）。
      // 表单只在提交成功或点「取消」时清（见 handleCreate / handleCancel）
      setSaving(false);
      setTimeout(() => nameRef.current?.focus(), 80);
    }
  }, [open]);

  /** 显式放弃：清草稿 + 清空表单——下次点「新建项目」是干净的空表单 */
  const handleCancel = () => {
    useModalDraftStore.getState().setDraft(CREATE_DRAFT_KEY, null);
    setName(''); setPath('');
    onClose();
  };

  const handleSelectPath = async () => {
    // 走后端原生选择器：路径会成为文件访问白名单的根，必须由用户亲手选择（见 utils/error 同层说明）
    const selected = await pickDirectory({ purpose: 'projectDir' });
    if (selected) setPath(selected);
  };

  const handleCreate = async () => {
    const n = name.trim();
    const p = path.trim();
    if (!n || !p) return;
    setSaving(true);
    try {
      const project = await projectApi.add(n, p);
      // 已落库：草稿使命完成（留着的话，下次点「新建项目」会把刚建过的内容带进来）
      useModalDraftStore.getState().setDraft(CREATE_DRAFT_KEY, null);
      setName(''); setPath('');
      onCreated(project);
      onClose();
    } catch (e: unknown) {
      // 后端会给出可行动的原因（路径不存在/危险目录/未授权），标题直接用原因更利于纠错
      reportError(toMessage(e) || '创建项目失败', e, { description: '' });
    }
    setSaving(false);
  };

  return (
    // onClose（遮罩 / Esc / ✕）＝ "先离开"而不是"放弃"：草稿留在 modalDraftStore 里（UX-14）
    <Modal open={open} title="新建项目" onClose={onClose}>
      <div className="space-y-4">
        <div>
          <label htmlFor={`${uid}-name`} className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">项目名称</label>
          <input
            id={`${uid}-name`}
            ref={nameRef}
            className="w-full px-3 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
            placeholder="例如: my-web-app"
            value={name}
            onChange={e => setName(e.target.value)}
            onKeyDown={e => { if (isSubmitEnter(e)) handleCreate(); }}
          />
        </div>
        <div>
          <label htmlFor={`${uid}-path`} className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">项目路径</label>
          <div className="relative">
            <input
              id={`${uid}-path`}
              className="w-full pl-3 pr-8 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text font-mono placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
              placeholder="/path/to/project"
              value={path}
              onChange={e => setPath(e.target.value)}
              onKeyDown={e => { if (isSubmitEnter(e)) handleCreate(); }}
            />
            <button
              aria-label="选择目录"
              className="absolute right-1 top-1/2 -translate-y-1/2 p-1 text-nexus-muted hover:text-nexus-text rounded"
              onClick={handleSelectPath}
            >
              <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3"><rect x="1" y="3" width="12" height="9" rx="1"/><path d="M1 5h12"/><path d="M5 1h2l1 2H5z"/></svg>
            </button>
          </div>
        </div>
        <div className="flex items-center justify-end gap-2 pt-1">
          <button
            className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
            onClick={handleCancel}
          >取消</button>
          <button
            className="px-6 py-1.5 text-[13px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover disabled:opacity-40 disabled:cursor-not-allowed"
            disabled={saving || !name.trim() || !path.trim()}
            onClick={handleCreate}
          >
            {saving ? '创建中…' : '创建项目'}
          </button>
        </div>
      </div>
    </Modal>
  );
}

interface EditModalProps {
  project: Project | null;
  onClose: () => void;
  onUpdated: () => void;
  onProjectName?: (name: string) => void;
}

export function EditProjectModal({ project, onClose, onUpdated, onProjectName }: EditModalProps) {
  const [name, setName] = useState('');
  const [path, setPath] = useState('');
  const [saving, setSaving] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);
  /** 表单字段 id 前缀（UX-11），同上 */
  const uid = useId();
  /** 上一次把哪个项目种进了表单（null = 已关闭）。见下方 effect 的说明 */
  const seededIdRef = useRef<string | null>(null);
  /** 草稿键随"正在编辑哪个项目"走：切到别的项目再切回来，各自的草稿都在 */
  const draftKey = project ? MODAL_DRAFT_KEYS.projectEdit(project.id) : null;
  // 与已持久化的值不同才算脏（与 ServiceEditPanel 的 formDirty 同口径）：改回原样自动清草稿
  const dirty = !!project && (name !== project.name || path !== project.path);
  /**
   * **在本次渲染里**固定住“表单当前属于哪个项目”。
   *
   * 不能直接在 effect 里读 ref：切换项目后的第一帧，`name/path` 还是上一个项目的值，
   * 而 seed effect 会把 ref 改成新 id；如果草稿 effect 在它之后跑（或读到改过的 ref），
   * 就会把旧项目的值写进新项目的草稿键，下一次初始化再读回来——用户看到“编辑 B
   * 显示 A 的信息”。用渲染期快照做闸门，效果顺序和 ref 变更都影响不到判断。
   */
  const seededId = seededIdRef.current;

  useEffect(() => {
    // 已关闭、或表单还没为当前项目种过值：不写草稿（见 seededId 的说明）
    if (!draftKey || !project || !shouldSyncProjectDraft(project.id, seededId)) return;
    useModalDraftStore.getState().setDraft(draftKey, dirty ? { name, path } : null);
  }, [draftKey, project, seededId, dirty, name, path]);

  // 用 layout effect：切换项目时在**绘制前**把表单换成新项目的值，
  // 避免先闪一帧上一个项目的名称/路径。
  useLayoutEffect(() => {
    if (!project) {
      seededIdRef.current = null; // 关闭：下次打开重新种一次（草稿这时才被读回来）
      return;
    }
    // 同一个项目再种一次会把用户正在改的内容冲掉：父级列表刷新（轮询/保存后 load）
    // 会给出**同一个 id 的新对象**，这里按 id 认人，不按对象身份
    if (seededIdRef.current === project.id) { setSaving(false); return; }
    seededIdRef.current = project.id;
    const draft = readModalDraft<ProjectFormDraft>(MODAL_DRAFT_KEYS.projectEdit(project.id));
    setName(draft?.name ?? project.name);
    setPath(draft?.path ?? project.path);
    setSaving(false);
    setTimeout(() => nameRef.current?.focus(), 80);
  }, [project]);

  /** 显式放弃：清草稿 + 回到已持久化的值（否则下次打开会把放弃过的编辑又"复活"） */
  const handleCancel = () => {
    if (draftKey) useModalDraftStore.getState().setDraft(draftKey, null);
    setName(project?.name ?? '');
    setPath(project?.path ?? '');
    onClose();
  };

  const handleSelectPath = async () => {
    const selected = await pickDirectory({ purpose: 'projectDir', defaultPath: path || project?.path });
    if (selected) setPath(selected);
  };

  const handleSave = async () => {
    const n = name.trim();
    const p = path.trim();
    if (!n || !p || !project) return;
    setSaving(true);
    try {
      await projectApi.update(project.id, n, p);
      // 已落库：草稿使命完成（留着的话下次打开会把刚保存过的编辑又"复活"回来）。
      // 表单同步成落库后的值：手输的首尾空格被 trim 掉了，不跟进的话 dirty 仍为真
      useModalDraftStore.getState().setDraft(MODAL_DRAFT_KEYS.projectEdit(project.id), null);
      setName(n); setPath(p);
      onProjectName?.(n);
      showNotification({ title: `已更新「${n}」` });
      onUpdated();
      onClose();
    } catch (e: unknown) {
      reportError('更新项目失败', e);
    }
    setSaving(false);
  };

  return (
    // onClose（遮罩 / Esc / ✕）＝ 草稿留着，重新打开同一个项目时恢复（UX-14）
    <Modal open={!!project} title="编辑项目" onClose={onClose}>
      <div className="space-y-4">
        <div>
          <label htmlFor={`${uid}-name`} className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">项目名称</label>
          <input
            id={`${uid}-name`}
            ref={nameRef}
            className="w-full px-3 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
            placeholder="例如: my-web-app"
            value={name}
            onChange={e => setName(e.target.value)}
            onKeyDown={e => { if (isSubmitEnter(e)) handleSave(); }}
          />
        </div>
        <div>
          <label htmlFor={`${uid}-path`} className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">项目路径</label>
          <div className="relative">
            <input
              id={`${uid}-path`}
              className="w-full pl-3 pr-8 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text font-mono placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
              placeholder="/path/to/project"
              value={path}
              onChange={e => setPath(e.target.value)}
              onKeyDown={e => { if (isSubmitEnter(e)) handleSave(); }}
            />
            <button
              aria-label="选择目录"
              className="absolute right-1 top-1/2 -translate-y-1/2 p-1 text-nexus-muted hover:text-nexus-text rounded"
              onClick={handleSelectPath}
            >
              <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3"><rect x="1" y="3" width="12" height="9" rx="1"/><path d="M1 5h12"/><path d="M5 1h2l1 2H5z"/></svg>
            </button>
          </div>
        </div>
        <div className="flex items-center justify-end gap-2 pt-1">
          <button
            className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
            onClick={handleCancel}
          >取消</button>
          <button
            className="px-6 py-1.5 text-[13px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover disabled:opacity-40 disabled:cursor-not-allowed"
            disabled={saving || !name.trim() || !path.trim()}
            onClick={handleSave}
          >{saving ? '保存中…' : '保存修改'}</button>
        </div>
      </div>
    </Modal>
  );
}

interface DeleteModalProps {
  target: { id: string; name: string } | null;
  onClose: () => void;
  onDeleted: (id: string) => void;
  /** 当删除的项目是当前选中项目时调用，用于取消选中 */
  onDeselectIfSelected?: () => void;
}

export function DeleteProjectModal({ target, onClose, onDeleted, onDeselectIfSelected }: DeleteModalProps) {
  const [deleting, setDeleting] = useState(false);

  const handleDelete = async () => {
    if (!target) return;
    setDeleting(true);
    try {
      const result = await projectApi.delete(target.id);
      onDeselectIfSelected?.();
      if (result.stop_warnings.length > 0) {
        showNotification({
          variant: 'warning',
          title: `已删除「${target.name}」，但有进程未能停止`,
          description: result.stop_warnings.join('；'),
          duration: 10000,
        });
      } else {
        showNotification({ variant: 'warning', title: `已删除「${target.name}」` });
      }
      onDeleted(target.id);
      onClose();
    } catch (e: unknown) {
      reportError(`删除项目「${target.name}」失败`, e);
    }
    setDeleting(false);
  };

  return (
    <Modal open={!!target} title="确认删除" onClose={onClose}>
      <div className="space-y-4">
        <p className="text-[13px] text-nexus-text">
          确定要删除项目 <span className="text-nexus-warning font-medium">「{target?.name}」</span> 吗？
        </p>
        <p className="text-[12px] text-nexus-muted">
          该项目下的所有服务配置也会被一并删除；正在运行的服务会先被停止。此操作不可撤销。
        </p>
        <div className="flex items-center justify-end gap-2">
          <button
            className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
            onClick={onClose}
          >取消</button>
          <button
            className="px-5 py-1.5 text-[13px] bg-nexus-error text-white rounded hover:bg-nexus-error/80 disabled:opacity-40"
            disabled={deleting}
            onClick={handleDelete}
          >{deleting ? '删除中…' : '确认删除'}</button>
        </div>
      </div>
    </Modal>
  );
}

interface DuplicateModalProps {
  target: { id: string; name: string } | null;
  onClose: () => void;
  onDuplicated: () => void;
}

export function DuplicateProjectModal({ target, onClose, onDuplicated }: DuplicateModalProps) {
  const [duplicating, setDuplicating] = useState(false);

  const handleDuplicate = async () => {
    if (!target) return;
    setDuplicating(true);
    try {
      const p = await projectApi.duplicate(target.id);
      showNotification({ title: `已复制为「${p.name}」` });
      onDuplicated();
      onClose();
    } catch (e: unknown) {
      reportError(`复制项目「${target.name}」失败`, e);
    }
    setDuplicating(false);
  };

  return (
    <Modal open={!!target} title="确认复制" onClose={onClose}>
      <div className="space-y-4">
        <p className="text-[13px] text-nexus-text">
          确定要复制项目 <span className="text-nexus-warning font-medium">「{target?.name}」</span> 吗？
        </p>
        <p className="text-[12px] text-nexus-muted">
          将复制该项目及其下所有服务配置，复制后的项目以「副本」命名。
        </p>
        <div className="flex items-center justify-end gap-2">
          <button
            className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
            onClick={onClose}
          >取消</button>
          <button
            className="px-5 py-1.5 text-[13px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover disabled:opacity-40"
            disabled={duplicating}
            onClick={handleDuplicate}
          >{duplicating ? '复制中…' : '确认复制'}</button>
        </div>
      </div>
    </Modal>
  );
}
