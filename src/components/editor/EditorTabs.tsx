import { useEditorStore, switchToTab } from '../../stores/editor';

export function EditorTabs() {
  const tabs = useEditorStore(s => s.tabs);
  const activeTabId = useEditorStore(s => s.activeTabId);
  const closeTab = useEditorStore(s => s.closeTab);

  if (tabs.length === 0) return null;

  return (
    <div className="flex bg-nexus-surface border-b border-nexus-border overflow-x-auto">
      {tabs.map(tab => {
        const isActive = activeTabId === tab.id;

        return (
          <div
            key={tab.id}
            className={`group flex items-center gap-1 px-3 h-[30px] cursor-pointer border-r border-nexus-border min-w-0 ${
              isActive
                ? 'bg-nexus-bg text-nexus-text'
                : 'text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover'
            }`}
            onClick={() => switchToTab(tab.id)}
          >
            <span className="truncate max-w-[160px] text-[13px]">{tab.name}</span>
            <button
              className="flex-shrink-0 w-[18px] h-[18px] flex items-center justify-center rounded opacity-0 group-hover:opacity-100 hover:bg-nexus-hover text-nexus-muted hover:text-nexus-text transition-all"
              onClick={(e) => { e.stopPropagation(); closeTab(tab.id); }}
            >
              <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round">
                <line x1="2" y1="2" x2="8" y2="8" /><line x1="8" y1="2" x2="2" y2="8" />
              </svg>
            </button>
          </div>
        );
      })}
    </div>
  );
}
