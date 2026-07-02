import { useState, useEffect, useRef } from 'react';
import { serviceApi } from '../../services/service';
import { open } from '@tauri-apps/plugin-dialog';
import { showNotification } from '../ui/Toast';

interface Props {
  projectId: string;
  projectPath: string;
  onDone: () => void;
}

export function AddServiceFormContent({ projectId, projectPath, onDone }: Props) {
  const [name, setName] = useState('');
  const [command, setCommand] = useState('');
  const [cwd, setCwd] = useState('');
  const [saving, setSaving] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);

  useEffect(() => { nameRef.current?.focus(); }, []);

  const handleSelectCwd = async () => {
    const selected = await open({ directory: true, title: '选择工作目录', defaultPath: cwd || projectPath });
    if (selected) setCwd(selected);
  };

  const handleSubmit = async () => {
    if (!name.trim()) return;
    setSaving(true);
    try {
      await serviceApi.add({
        projectId, name: name.trim(), command, cwd,
        watchPaths: '[]', envVars: '', restartMode: 0,
        toolCommands: '[]',
      });
      onDone();
    } catch (e: unknown) { console.error('添加服务失败:', e); showNotification({ variant: 'error', title: '添加服务失败', description: String(e) }); }
    setSaving(false);
  };

  return (
    <div className="space-y-4">
      <div>
        <label className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">服务名称</label>
        <input
          ref={nameRef}
          className="w-full px-3 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
          placeholder="例如: backend, frontend, api-server"
          value={name}
          onChange={e => setName(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter') handleSubmit(); }}
        />
      </div>
      <div>
        <label className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">启动命令</label>
        <input
          className="w-full px-3 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text font-mono placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
          placeholder="npm run dev / cargo run / python main.py"
          value={command}
          onChange={e => setCommand(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter') handleSubmit(); }}
        />
      </div>
      <div>
        <label className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">
          工作目录
        </label>
        <div className="relative">
          <input
            className="w-full pl-3 pr-8 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text font-mono placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
            placeholder="/path/to/service"
            value={cwd}
            onChange={e => setCwd(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter') handleSubmit(); }}
          />
          <button
            className="absolute right-1 top-1/2 -translate-y-1/2 p-1 text-nexus-muted hover:text-nexus-text rounded"
            onClick={handleSelectCwd}
          >
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3"><rect x="1" y="3" width="12" height="9" rx="1"/><path d="M1 5h12"/><path d="M5 1h2l1 2H5z"/></svg>
          </button>
        </div>
      </div>
      <div className="flex items-center justify-end gap-2 pt-1">
        <button
          className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
          onClick={onDone}
        >取消</button>
        <button
          className="px-6 py-1.5 text-[13px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover disabled:opacity-40 disabled:cursor-not-allowed"
          disabled={saving || !name.trim()}
          onClick={handleSubmit}
        >{saving ? '创建中…' : '添加服务'}</button>
      </div>
    </div>
  );
}
