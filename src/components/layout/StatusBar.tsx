import { useMemo } from 'react';
import { useEditorStore } from '../../stores/editor';

interface StatusBarProps {
  showTerminal: boolean;
  onToggleTerminal: () => void;
}

export function StatusBar({ showTerminal, onToggleTerminal }: StatusBarProps) {
  const activeTabId = useEditorStore(s => s.activeTabId);
  const tabs = useEditorStore(s => s.tabs);
  const activeTab = useMemo(() => tabs.find(t => t.id === activeTabId), [tabs, activeTabId]);

  return (
    <div className="h-[22px] bg-nexus-surface border-t border-nexus-border flex items-center justify-between px-2 text-[11px] text-nexus-text-muted flex-shrink-0">
      {/* 左侧：终端按钮 */}
      <button
        className={`px-1.5 py-0.5 rounded transition-colors ${
          showTerminal
            ? 'bg-nexus-hover text-nexus-text'
            : 'hover:bg-nexus-hover/50 text-nexus-text-muted hover:text-nexus-text'
        }`}
        onClick={onToggleTerminal}
      >
        终端
      </button>

      {/* 右侧：文件路径 */}
      <div className="flex items-center gap-3 overflow-hidden">
        {activeTab && <span className="truncate max-w-[300px]">{activeTab.path}</span>}
      </div>
    </div>
  );
}
