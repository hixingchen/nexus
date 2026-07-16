import { useMemo } from 'react';
import { useEditorStore } from '../../stores/editor';

interface StatusBarProps {
  showTerminal: boolean;
  onToggleTerminal: () => void;
  activeBottomTab?: 'terminal' | 'claude';
  onSwitchBottomTab?: (tab: 'terminal' | 'claude') => void;
}

export function StatusBar({ showTerminal, onToggleTerminal, activeBottomTab, onSwitchBottomTab }: StatusBarProps) {
  const activeTabId = useEditorStore(s => s.activeTabId);
  const tabs = useEditorStore(s => s.tabs);
  const activeTab = useMemo(() => tabs.find(t => t.id === activeTabId), [tabs, activeTabId]);

  return (
    <div className="h-[22px] bg-nexus-surface border-t border-nexus-border flex items-center justify-between px-2 text-[11px] text-nexus-text-muted flex-shrink-0">
      {/* 左侧：终端和 Claude Code 按钮 */}
      <div className="flex items-center gap-0.5">
        <button
          className={`px-1.5 py-0.5 rounded transition-colors ${
            showTerminal && activeBottomTab === 'terminal'
              ? 'bg-nexus-hover text-nexus-text'
              : 'hover:bg-nexus-hover/50 text-nexus-text-muted hover:text-nexus-text'
          }`}
          onClick={() => {
            if (showTerminal && activeBottomTab === 'terminal') {
              onToggleTerminal();
            } else if (!showTerminal) {
              onSwitchBottomTab?.('terminal');
              onToggleTerminal();
            } else {
              onSwitchBottomTab?.('terminal');
            }
          }}
        >
          终端
        </button>
        <button
          className={`flex items-center gap-1 px-1.5 py-0.5 rounded transition-colors ${
            showTerminal && activeBottomTab === 'claude'
              ? 'bg-nexus-hover text-nexus-text'
              : 'hover:bg-nexus-hover/50 text-nexus-text-muted hover:text-nexus-text'
          }`}
          onClick={() => {
            if (showTerminal && activeBottomTab === 'claude') {
              onToggleTerminal();
            } else if (!showTerminal) {
              onSwitchBottomTab?.('claude');
              onToggleTerminal();
            } else {
              onSwitchBottomTab?.('claude');
            }
          }}
        >
          <span className="text-[10px]">✦</span>
          Claude Code
        </button>
      </div>

      {/* 右侧：文件路径 */}
      <div className="flex items-center gap-3 overflow-hidden">
        {activeTab && <span className="truncate max-w-[300px]">{activeTab.path}</span>}
      </div>
    </div>
  );
}
