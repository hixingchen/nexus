import { getCurrentWindow } from '@tauri-apps/api/window';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { reportError } from '../../utils/error';
import { notify } from '../../utils/notify';
import { compareVersions } from '../../utils/version';
import { nodeApi } from '../../services/service';
import { getAppVersion } from '../../services/system';
import { NodeVersionModal } from './NodeVersionModal';
import { useCallback, useEffect, useState, useRef } from 'react';
import { useClickOutside } from '../../hooks/useClickOutside';

/** 仓库地址：关于与检查更新都从这里取（换仓库时只改这一处） */
const REPO_URL = 'https://github.com/hixingchen/nexus';
const LATEST_RELEASE_API = 'https://api.github.com/repos/hixingchen/nexus/releases/latest';

interface TitleBarProps {
  projectName?: string | null;
}

export function TitleBar({ projectName }: TitleBarProps) {
  const appWindow = getCurrentWindow();
  const [isMaximized, setIsMaximized] = useState(false);
  /**
   * Node 版本面板的开关与刷新信号，提到这一层——**因为 Modal 必须渲染在下面那个
   * 带拖拽 handler 的 div 之外**。
   *
   * 踩过的坑：Modal 之前挂在这个 div 里面，于是点它的非按钮区域（背景、文字）时，
   * mousedown 冒泡到标题栏 → 触发 `startDragging()`，**点面板会把整个窗口拖走**
   * （用户报过）。拖拽判定只跳过 `button, [data-menu]`，而 Modal 的绝大部分区域两者都不是。
   */
  const [nodePanelOpen, setNodePanelOpen] = useState(false);
  /** 面板里切换过版本就 +1，让按钮上那个版本号跟着变（此刻面板还开着，不能等它关闭） */
  const [nodeRefreshSeq, setNodeRefreshSeq] = useState(0);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    // 最大化状态查询失败不影响使用（保持默认 false），但必须消费掉 rejection——
    // 裸 .then 会抛未处理的 rejection，且窗口尺寸变化后状态会一直不同步
    const syncMaximized = () => {
      appWindow.isMaximized().then(v => { if (!disposed) setIsMaximized(v); })
        // 只留控制台：影响面是标题栏图标的最大化/还原外观，弹 toast 属噪音
        .catch(e => console.error('查询窗口最大化状态失败:', e));
    };
    syncMaximized();
    appWindow.onResized(syncMaximized)
      .then(fn => { if (disposed) { fn(); return; } unlisten = fn; })
      // 同上：订阅失败只是图标不再自动同步，用户仍能点按钮切换
      .catch(e => console.error('订阅窗口尺寸变化失败:', e));
    return () => { disposed = true; unlisten?.(); };
  }, [appWindow]);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    // 收窄而不是断言（CQ-21）：mousedown 的 target 可能是**文本节点**（点中的是标题栏
    // 里的文字），此时 `.closest` 为 undefined —— `as HTMLElement` 会让它一路跑到
    // "closest is not a function" 的 TypeError，把标题栏拖拽整个打断。
    // 非元素目标一律按"没点在按钮/菜单上"处理（与点空白处一致）。
    const target = e.target;
    if (target instanceof Element && target.closest('button, [data-menu]')) return;
    appWindow.startDragging().catch(() => { /* 非 mousedown 上下文等场景下拖动被拒绝，忽略 */ });
  }, [appWindow]);

  return (
    <>
    <div className="h-[32px] bg-nexus-titlebar flex items-center select-none flex-shrink-0" onMouseDown={handleMouseDown}>
      {/* ── 左上角：只放"这是哪个应用" ──
          工具一律排到右边（见下面那段）。左角塞第二件东西的代价不只是难看：左列宽度一变，
          中间的项目名就跟着左右漂——`Node v22.22.1` 和没装 nvm 时的 `Node` 差 9 个字符 */}
      <AppMenu />

      {/* ── 项目名 ── */}
      <div className="flex-1 flex items-center justify-center h-full">
        <span className="text-[12px] text-nexus-text-muted">
          {projectName || ''}
        </span>
      </div>

      {/* ── 右侧：工具 ｜ 窗口控制 ──
          竖线画在这里才有实义：左边是应用自己的功能，右边是系统的窗口按钮。
          （之前它画在「Nexus」和「Node」之间，隔开的是两件同类的东西，等于没分组） */}
      <NodeVersionButton onOpen={() => setNodePanelOpen(true)} refreshSeq={nodeRefreshSeq} />
      <div className="w-px h-[14px] bg-nexus-border flex-shrink-0 mx-1.5" />

      {/* ── 窗口控制 ── */}
      <div className="flex h-full">
        <button className="w-[46px] h-full flex items-center justify-center text-nexus-muted hover:bg-nexus-hover hover:text-nexus-text" aria-label="最小化" title="最小化" onClick={() => appWindow.minimize()}>
          <svg width="10" height="1" viewBox="0 0 10 1" fill="currentColor"><rect width="10" height="1"/></svg>
        </button>
        <button className="w-[46px] h-full flex items-center justify-center text-nexus-muted hover:bg-nexus-hover hover:text-nexus-text" onClick={() => appWindow.toggleMaximize()} title={isMaximized ? "还原" : "最大化"}>
          {isMaximized ? (
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1"><rect x="2" y="0" width="7" height="7"/><polyline points="0,3 0,10 7,10"/></svg>
          ) : (
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1"><rect x="0.5" y="0.5" width="9" height="9"/></svg>
          )}
        </button>
        <button className="w-[46px] h-full flex items-center justify-center text-nexus-muted hover:bg-nexus-error hover:text-white" aria-label="关闭窗口" title="关闭窗口" onClick={() => appWindow.close()}>
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2"><line x1="1" y1="1" x2="9" y2="9"/><line x1="9" y1="1" x2="1" y2="9"/></svg>
        </button>
      </div>
    </div>

    {/* 在标题栏的拖拽区**之外**（见 nodePanelOpen 的说明），也因此在窗口按钮那一行之下 */}
    <NodeVersionModal
      open={nodePanelOpen}
      onClose={() => setNodePanelOpen(false)}
      onChanged={() => setNodeRefreshSeq(s => s + 1)}
    />
    </>
  );
}

// ─── 应用菜单 ──────────────────────────────────────────

/**
 * 左上角的应用菜单。
 *
 * 从「帮助」改名而来：那里此前只有一项「关于 Nexus」，没有任何帮助内容，名字与内容
 * 对不上；加上「检查更新」之后装的是清一色的**应用级信息**，用应用名更准。
 * 以后要放「使用文档」「反馈问题」这类真正的帮助入口，再加回来也不冲突。
 */
function AppMenu() {
  const [open, setOpen] = useState(false);
  /** 检查更新进行中（菜单项显示"检查中…"并禁用，避免连点发出一串请求） */
  const [checking, setChecking] = useState(false);
  /**
   * 应用自身版本，显示在下面「检查更新」那一行的右侧。
   *
   * 为什么挂这儿：它是"你装的到底是哪一版"这个问题的答案，而这个问题在报障、对照发布
   * 说明时会被反复问起——挨着更新入口放，问的人和答的人看的是同一处。不重复写到标题栏
   * 按钮上，同一屏就不必出现两段版本信息（旁边那个「Node x.y.z」也不是同一个东西）。
   */
  const [appVersion, setAppVersion] = useState('');
  const ref = useRef<HTMLDivElement>(null);

  // 点击菜单外部关闭（菜单内的点击由各自按钮处理）
  useClickOutside(ref, () => setOpen(false));

  useEffect(() => {
    let alive = true;
    getAppVersion()
      .then(v => { if (alive) setAppVersion(v); })
      .catch(() => { if (alive) setAppVersion(''); });   // 读不到就只显示 Nexus，不报错
    return () => { alive = false; };
  }, []);

  /**
   * 检查更新：拉 GitHub 的最新 release，与本机版本比。
   *
   * 版本号问后端要（`CARGO_PKG_VERSION`）而不是读 `package.json`——后者是 npm 侧的，
   * 与这个二进制未必同步；比的是"用户装的这一份"有没有新版，就得用它自己的版本。
   *
   * 三种结果都要说清楚，尤其"比不出来"：把它和"已是最新"混为一谈的话，界面会把一个
   * 解析失败说成结论。
   */
  const handleCheckUpdate = async (e: React.MouseEvent) => {
    e.stopPropagation();
    setOpen(false);
    setChecking(true);
    try {
      const current = await getAppVersion();
      const res = await fetch(LATEST_RELEASE_API, { headers: { Accept: 'application/vnd.github+json' } });
      if (!res.ok) throw new Error(`GitHub 返回 ${res.status}`);
      const data = (await res.json()) as { tag_name?: string; html_url?: string };
      const latest = (data.tag_name ?? '').trim();
      const cmp = compareVersions(latest, current);

      if (cmp === null) {
        notify({
          variant: 'warning',
          title: '无法比较版本号',
          description: `本机 ${current}，线上 ${latest || '(没拿到 tag)'}`,
          duration: 6000,
        });
      } else if (cmp > 0) {
        notify({
          variant: 'info',
          title: `有新版本 ${latest}`,
          description: `当前 ${current}`,
          duration: 10000,
          // 提示里直接给出口：让用户读完再去别处找下载页，等于把"应用知道怎么办"
          // 降级成"用户自己去想"
          action: {
            label: '打开下载页',
            run: () => { void openUrl(data.html_url || `${REPO_URL}/releases`).catch((err) => reportError('打开网页失败', err)); },
          },
        });
      } else {
        notify({ title: '已是最新版本', description: `当前 ${current}`, duration: 2500 });
      }
    } catch (err) {
      // 网络不通 / GitHub 限流 / CSP 没放开——原因都在 err 里，别只说"失败"
      reportError('检查更新失败', err);
    } finally {
      setChecking(false);
    }
  };

  return (
    <div className="relative h-full" ref={ref}>
      <button
        className={`h-full px-2.5 text-[12.5px] transition-colors ${
          open ? 'bg-nexus-hover text-nexus-text' : 'text-nexus-text-muted hover:text-nexus-text hover:bg-nexus-hover/40'
        }`}
        onClick={(e) => { e.stopPropagation(); setOpen(!open); }}
      >
        Nexus
      </button>
      {open && (
        <div className="absolute top-full left-0 mt-0.5 min-w-[160px] bg-nexus-surface border border-nexus-border rounded-md shadow-xl py-1 z-50">
          <button
            className="w-full px-3 py-1.5 text-[12px] text-nexus-text hover:bg-nexus-accent hover:text-white text-left transition-colors"
            onClick={async (e) => {
              e.stopPropagation();
              setOpen(false);
              try {
                await openUrl(REPO_URL);
              } catch (err) {
                reportError('打开网页失败', err);
              }
            }}
          >
            关于 Nexus
          </button>
          <button
            className="group w-full flex items-center justify-between gap-3 px-3 py-1.5 text-[12px] text-nexus-text hover:bg-nexus-accent hover:text-white text-left transition-colors disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-nexus-text"
            disabled={checking}
            onClick={handleCheckUpdate}
          >
            <span>{checking ? '检查中…' : '检查更新'}</span>
            {/* group（parent）是为了让版本号跟着这一行变亮：显式写了 text-* 的元素
                不继承父级的 hover:text-white，得自己接 group-hover */}
            {appVersion && (
              // 字号与「检查更新」一致（继承父级 12px）：小一号看着像另一种信息，
              // 而这两段是同一句话的两半——"检查更新（当前 v1.2.0）"
              <span className="flex-shrink-0 text-nexus-text-muted group-hover:text-white/85 transition-colors">
                v{appVersion}
              </span>
            )}
          </button>
        </div>
      )}
    </div>
  );
}

/**
 * 标题栏右侧的 Node 版本入口（在窗口按钮左边，与它们之间隔一条竖线）。
 *
 * 为什么在右边：标题栏左角是"这是哪个应用"的位置，工具排右边是 Windows 上的通例
 * （Chrome 的扩展图标、VS Code 的布局按钮都在这个位置），也让左列宽度固定下来——
 * 否则项目名会随这里的版本号长短左右漂。AI 面板头部那排工具图标也是同样的理由。
 *
 * 为什么单独立一个按钮、而不是当菜单项：那个菜单装的是**关于这个应用**的东西
 * （关于 / 检查更新），而这是个**工具**。混在一起有两个代价——菜单随功能增加越来越长，
 * 而且每开一个工具都要"点菜单 → 点条目"两步。
 *
 * 样式上做成圆角小控件（而不是像窗口按钮那样通栏高亮）：它和应用菜单是同一层级的东西，
 * 只因为"身份在左、工具在右"才分居两端；跟系统的三个窗口按钮不是一类。
 */
function NodeVersionButton({ onOpen, refreshSeq }: {
  onOpen: () => void;
  /**
   * 外部要求刷新的信号（面板里切换过版本时 +1）。
   *
   * 为什么不是"面板关闭时再读一次"就够：用户切完之后面板**还开着**（很可能继续看列表），
   * 而按钮上那个版本号这时就已经是旧的了——用户报过"点了设为当前界面状态没变化"。
   */
  refreshSeq: number;
}) {
  /** 当前版本（空 = 没装 nvm 或没设置）；直接写在按钮上，省一次点击 */
  const [version, setVersion] = useState('');

  // 读本地 settings.txt + 软链，毫秒级、不起进程。不轮询：它只在用户自己动手时才变
  const refresh = useCallback(async () => {
    try {
      const rt = await nodeApi.getRuntime();
      setVersion(rt.available ? rt.current : '');
    } catch {
      setVersion('');   // 读不到就不显示版本号，按钮本身照常可用
    }
  }, []);

  useEffect(() => { void refresh(); }, [refresh, refreshSeq]);

  return (
    <button
      className="h-[22px] px-2 rounded flex items-center flex-shrink-0 text-[12.5px] text-nexus-text-muted hover:text-nexus-text hover:bg-nexus-hover transition-colors"
      title={version ? `${version} · 点击管理 Node 版本` : '管理 Node 版本'}
      onClick={(e) => { e.stopPropagation(); onOpen(); }}
    >
      {/* 不用图标：一个六边形说明不了"这是 Node"（那是只有熟悉的人认得的形状），
          而"Node"三个字母谁都看得懂——工具入口先要说清自己是什么 */}
      Node{version ? ` ${version}` : ''}
    </button>
  );
}
