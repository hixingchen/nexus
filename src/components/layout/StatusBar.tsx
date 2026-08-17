import { useMemo } from 'react';
import { useEditorStore } from '../../stores/editor';

export function StatusBar() {
  const activeTabId = useEditorStore(s => s.activeTabId);
  const tabs = useEditorStore(s => s.tabs);
  const activeTab = useMemo(() => tabs.find(t => t.id === activeTabId), [tabs, activeTabId]);

  return (
    <div className="h-[22px] bg-nexus-surface border-t border-nexus-border flex items-center justify-end px-2 text-[11px] text-nexus-text-muted flex-shrink-0">
      <div className="flex items-center gap-3 overflow-hidden">
        {activeTab && <span className="truncate max-w-[300px]">{activeTab.path}</span>}
      </div>
    </div>
  );
}
