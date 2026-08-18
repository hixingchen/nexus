import { useState, useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { Modal } from '../ui/Modal';
import { SvgIcon } from '../ui/SvgIcon';
import { getIconSvg } from '../file-tree/FileIcons';
import { useEditorStore, switchToTab, saveActiveFile } from '../../stores/editor';
import type { FileTab } from '../../types/editor';

export function EditorTabs() {
  const tabs = useEditorStore(s => s.tabs);
  const activeTabId = useEditorStore(s => s.activeTabId);
  const dirtyIds = useEditorStore(s => s.dirtyIds);
  const closeTab = useEditorStore(s => s.closeTab);
  const closeTabs = useEditorStore(s => s.closeTabs);
  const activeTab = tabs.find(t => t.id === activeTabId) ?? null;
  /** 单标签关闭确认（未保存） */
  const [confirmTarget, setConfirmTarget] = useState<FileTab | null>(null);
  /** 右键菜单 */
  const [tabMenu, setTabMenu] = useState<{ x: number; y: number; tab: FileTab } | null>(null);
  /** 批量关闭确认（含未保存标签时） */
  const [confirmMany, setConfirmMany] = useState<{ title: string; ids: string[]; note: string } | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  // 右键菜单：mousedown 外部点击关闭（菜单内不关，否则菜单项点击失效）
  useEffect(() => {
    if (!tabMenu) return;
    const h = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setTabMenu(null);
    };
    document.addEventListener('mousedown', h);
    return () => document.removeEventListener('mousedown', h);
  }, [tabMenu]);

  // 单标签关闭：未保存先弹确认
  const handleCloseClick = (e: React.MouseEvent, tab: FileTab) => {
    e.stopPropagation();
    if (dirtyIds.includes(tab.id)) setConfirmTarget(tab);
    else closeTab(tab.id);
  };

  // 批量关闭请求：含未保存标签时先弹确认，否则直接关
  const requestCloseMany = (title: string, ids: string[], dirtyCount: number) => {
    setTabMenu(null);
    if (ids.length === 0) return;
    if (dirtyCount > 0) {
      setConfirmMany({ title, ids, note: `其中 ${dirtyCount} 个文件有未保存的更改，关闭后更改将丢失。` });
    } else {
      closeTabs(ids);
    }
  };

  // ── 右键菜单操作 ──
  const handleMenuClose = (tab: FileTab) => {
    setTabMenu(null);
    if (dirtyIds.includes(tab.id)) setConfirmTarget(tab);
    else closeTab(tab.id);
  };
  const handleMenuCloseOthers = (tab: FileTab) => {
    const others = tabs.filter(t => t.id !== tab.id);
    requestCloseMany('关闭其他标签', others.map(t => t.id), others.filter(t => dirtyIds.includes(t.id)).length);
  };
  const handleMenuCloseRight = (tab: FileTab) => {
    const idx = tabs.findIndex(t => t.id === tab.id);
    if (idx === -1) return;
    const right = tabs.slice(idx + 1);
    requestCloseMany('关闭右侧标签', right.map(t => t.id), right.filter(t => dirtyIds.includes(t.id)).length);
  };
  const handleMenuCloseLeft = (tab: FileTab) => {
    const idx = tabs.findIndex(t => t.id === tab.id);
    if (idx <= 0) return;
    const left = tabs.slice(0, idx);
    requestCloseMany('关闭左侧标签', left.map(t => t.id), left.filter(t => dirtyIds.includes(t.id)).length);
  };
  const handleMenuCloseAll = () => {
    requestCloseMany('关闭所有标签', tabs.map(t => t.id), dirtyIds.length);
  };
  const handleMenuCloseSaved = () => {
    setTabMenu(null);
    closeTabs(tabs.filter(t => !dirtyIds.includes(t.id)).map(t => t.id));
  };

  if (tabs.length === 0) return null;

  const menuItemCls = "w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors text-left disabled:opacity-40 disabled:hover:bg-transparent";
  const lastTabId = tabs[tabs.length - 1].id;

  return (
    <>
      <div className="flex bg-nexus-surface border-b border-nexus-border overflow-x-auto">
        {tabs.map(tab => {
          const isActive = activeTabId === tab.id;
          const isDirty = dirtyIds.includes(tab.id);
          const ext = tab.name.split('.').pop() ?? '';

          return (
            <div
              key={tab.id}
              className={`group relative flex items-center gap-1.5 pl-3 pr-2 h-[34px] cursor-pointer border-r border-nexus-border min-w-0 transition-colors ${
                isActive
                  ? 'bg-nexus-bg text-nexus-text'
                  : 'text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/40'
              }`}
              onClick={() => switchToTab(tab.id)}
              onAuxClick={(e) => { if (e.button === 1) { e.preventDefault(); handleCloseClick(e, tab); } }}
              onContextMenu={(e) => { e.preventDefault(); setTabMenu({ x: e.clientX, y: e.clientY, tab }); }}
              title={tab.path}
            >
              {/* 激活标签顶部指示条 */}
              {isActive && <span className="absolute left-0 right-0 top-0 h-[2px] bg-nexus-accent" />}

              {/* 文件类型图标 */}
              <SvgIcon
                svg={getIconSvg(ext)}
                className="flex-shrink-0"
                style={{ width: 14, height: 14 }}
              />

              <span className="truncate max-w-[140px] text-[12.5px]">{tab.name}</span>

              {/* 未保存圆点（hover 时变为关闭按钮） */}
              {isDirty && (
                <span
                  className="w-[8px] h-[8px] rounded-full bg-nexus-accent flex-shrink-0 group-hover:hidden transition-opacity"
                  title="未保存"
                />
              )}
              <button
                className={`flex-shrink-0 w-[18px] h-[18px] flex items-center justify-center rounded-full hover:bg-nexus-error/10 text-nexus-muted hover:text-nexus-error transition-all ${
                  isActive ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'
                }`}
                onClick={(e) => handleCloseClick(e, tab)}
                title={isDirty ? '关闭（未保存的更改将丢失）' : '关闭（或中键点击标签）'}
              >
                <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round">
                  <line x1="2" y1="2" x2="8" y2="8" /><line x1="8" y1="2" x2="2" y2="8" />
                </svg>
              </button>
            </div>
          );
        })}

        {/* 右侧：保存当前文件（有未保存更改且非只读时可用） */}
        <div className="flex items-center ml-auto px-2 flex-shrink-0">
          <button
            className="p-1 rounded text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover disabled:opacity-40 disabled:hover:bg-transparent disabled:hover:text-nexus-muted transition-colors"
            title={activeTab?.readonly ? '文件过大，仅支持查看' : '保存当前文件（Ctrl+S）'}
            disabled={dirtyIds.length === 0 || !!activeTab?.readonly}
            onClick={() => { void saveActiveFile(); }}
          >
            <svg width="13" height="13" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.2">
              <path d="M1.5 1.5h6.5l2.5 2.5v6.5h-9v-9z"/>
              <path d="M3.5 1.5v3h4v-3"/>
              <path d="M3.5 10.5V7h5v3.5"/>
            </svg>
          </button>
        </div>
      </div>

      {/* 标签右键菜单：关闭功能 */}
      {tabMenu && createPortal(
        <div
          ref={menuRef}
          className="fixed z-[70] w-[170px] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden"
          style={{
            left: Math.min(tabMenu.x, window.innerWidth - 178),
            top: Math.min(tabMenu.y, window.innerHeight - 220),
          }}
        >
          {/* 在目录树中定位（主操作，独立一组） */}
          <div className="py-1.5 px-1.5">
            <button
              className={menuItemCls}
              onClick={() => {
                useEditorStore.getState().requestReveal(tabMenu.tab.path);
                setTabMenu(null);
              }}
            >
              <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round" className="text-nexus-muted group-hover:text-nexus-accent flex-shrink-0">
                <path d="M6 1.5a4 4 0 00-4 4c0 2.8 4 5.5 4 5.5s4-2.7 4-5.5a4 4 0 00-4-4z"/>
                <circle cx="6" cy="5.4" r="1.4"/>
              </svg>
              <span className="text-[12px] text-nexus-text">在目录树中定位</span>
            </button>
          </div>

          {/* 关闭功能组 */}
          <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
            <button className={menuItemCls} onClick={() => handleMenuClose(tabMenu.tab)}>
              <span className="text-[12px] text-nexus-text">关闭</span>
            </button>
            <button className={menuItemCls} disabled={tabs.length <= 1} onClick={() => handleMenuCloseOthers(tabMenu.tab)}>
              <span className="text-[12px] text-nexus-text">关闭其他</span>
            </button>
            <button className={menuItemCls} disabled={lastTabId === tabMenu.tab.id} onClick={() => handleMenuCloseRight(tabMenu.tab)}>
              <span className="text-[12px] text-nexus-text">关闭右侧</span>
            </button>
            <button className={menuItemCls} disabled={tabs[0].id === tabMenu.tab.id} onClick={() => handleMenuCloseLeft(tabMenu.tab)}>
              <span className="text-[12px] text-nexus-text">关闭左侧</span>
            </button>
          </div>
          <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
            <button className={menuItemCls} disabled={!tabs.some(t => !dirtyIds.includes(t.id))} onClick={handleMenuCloseSaved}>
              <span className="text-[12px] text-nexus-text">关闭已保存</span>
            </button>
            <button className={menuItemCls} onClick={handleMenuCloseAll}>
              <span className="text-[12px] text-nexus-text">关闭所有</span>
            </button>
          </div>
        </div>,
        document.body,
      )}

      {/* 关闭未保存标签确认 */}
      <Modal open={!!confirmTarget} title="未保存的更改" onClose={() => setConfirmTarget(null)}>
        <div className="space-y-4">
          <p className="text-[13px] text-nexus-text">
            文件 <span className="text-nexus-warning font-medium">「{confirmTarget?.name}」</span> 有未保存的更改，确定要关闭吗？
          </p>
          <p className="text-[12px] text-nexus-muted">关闭后将丢失未保存的修改。</p>
          <div className="flex items-center justify-end gap-2">
            <button
              className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
              onClick={() => setConfirmTarget(null)}
            >取消</button>
            <button
              className="px-5 py-1.5 text-[13px] bg-nexus-error text-white rounded hover:bg-nexus-error/80"
              onClick={() => { if (confirmTarget) closeTab(confirmTarget.id); setConfirmTarget(null); }}
            >放弃更改并关闭</button>
          </div>
        </div>
      </Modal>

      {/* 批量关闭确认（含未保存标签） */}
      <Modal open={!!confirmMany} title={confirmMany?.title ?? ''} onClose={() => setConfirmMany(null)}>
        <div className="space-y-4">
          <p className="text-[13px] text-nexus-text">{confirmMany?.note}</p>
          <p className="text-[12px] text-nexus-muted">此操作不可撤销。</p>
          <div className="flex items-center justify-end gap-2">
            <button
              className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
              onClick={() => setConfirmMany(null)}
            >取消</button>
            <button
              className="px-5 py-1.5 text-[13px] bg-nexus-error text-white rounded hover:bg-nexus-error/80"
              onClick={() => { if (confirmMany) closeTabs(confirmMany.ids); setConfirmMany(null); }}
            >确认关闭</button>
          </div>
        </div>
      </Modal>
    </>
  );
}
