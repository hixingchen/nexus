import { useToolCommandStore } from '../../stores/toolCommandStore';
import { useAiPanelReserve } from '../../hooks/useAiPanelReserve';

/**
 * 后台工具命令指示器（右下角、状态栏上方）。
 *
 * 为什么需要它：执行弹窗可以被关掉，而关闭**不会**停止命令。没有这个常驻入口时，
 * 用户关掉窗口后既看不到命令还在跑，也没有地方把它再打开/停止——只能凭记忆猜，
 * 然后可能再执行一遍（两条 npm install 互踩）。这里把"还在跑"变成可见状态：
 * 点击任意一条即重新打开它的实时输出弹窗。
 *
 * 只展示**后台**运行中的命令（弹窗正开着的那条不重复显示）；已结束的由完成通知告知。
 */
export function ToolCommandIndicator() {
  const runs = useToolCommandStore(s => s.runs);
  const openRunId = useToolCommandStore(s => s.openRunId);
  const open = useToolCommandStore(s => s.open);
  // AI 面板里是原生子 WebView，永远画在 DOM 之上——不按面板宽度让位就会整块被盖住
  const aiPanelReserve = useAiPanelReserve();

  const background = Object.values(runs).filter(r => r.loading && r.runId !== openRunId);
  if (background.length === 0) return null;

  return (
    <div
      className="fixed bottom-[30px] z-[55] flex flex-col items-end gap-1.5"
      style={{ right: aiPanelReserve + 12 }}
    >
      {background.map(run => (
        <button
          key={run.runId}
          onClick={() => open(run.runId)}
          title="命令在后台继续运行 · 点击查看实时输出"
          className="flex items-center gap-2 max-w-[320px] px-3 py-1.5 rounded-md border border-nexus-warning/40 bg-nexus-surface shadow-lg text-[11px] text-nexus-text hover:bg-nexus-hover/60 transition-colors"
        >
          <span className="w-[7px] h-[7px] rounded-full bg-nexus-warning animate-pulse flex-shrink-0" />
          <span className="truncate font-medium">{run.commandName}</span>
          <span className="text-nexus-muted flex-shrink-0">运行中 · 点击查看</span>
        </button>
      ))}
    </div>
  );
}
