import { getCurrentWindow } from '@tauri-apps/api/window';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { reportError } from '../../utils/error';
import { notify } from '../../utils/notify';
import { compareVersions } from '../../utils/version';
import { nodeApi } from '../../services/service';
import { getAppVersion } from '../../services/system';
import { LATEST_RELEASE_API, NEW_ISSUE_URL, REPO_URL } from '../../utils/links';
import { SettingsModal, type SettingsSection } from '../settings/SettingsModal';
import { AboutModal } from '../help/AboutModal';
import { UsageDocsModal } from '../help/UsageDocsModal';
import { useCallback, useEffect, useState, useRef } from 'react';
import { useClickOutside } from '../../hooks/useClickOutside';

/**
 * 标题栏左上角的两个顶级菜单。
 *
 * 「设置」「帮助」这个组合是 Windows 上的通例（帮助永远排最后），而菜单里装什么见各自的
 * 注释。这里只记一条**曾经的教训**：这块位置一度叫「帮助」但里面只有「关于 Nexus」，
 * 名字与内容对不上，于是被改成了应用名「Nexus」；现在「帮助」回来，是因为它下面真的有了
 * 帮助内容（使用文档 / 报告问题），而不只是应用信息——名字与内容重新对上了。
 */
type TopMenu = 'settings' | 'help';

/** 帮助菜单里**开自己弹窗**的条目（报告问题 / 检查更新是直接动作，不在此列） */
type HelpDialog = 'docs' | 'about';

interface TitleBarProps {
  projectName?: string | null;
}

export function TitleBar({ projectName }: TitleBarProps) {
  const appWindow = getCurrentWindow();
  const [isMaximized, setIsMaximized] = useState(false);
  /**
   * 当前展开的顶级菜单（null = 都关着）。
   *
   * **为什么要提到这一层**：两个菜单各自管自己的开关时，先点「设置」再点「帮助」，
   * 两个下拉会同时挂在标题栏下面（互不知情）。状态只有一个，就不存在这个可能。
   */
  const [openMenu, setOpenMenu] = useState<TopMenu | null>(null);
  /**
   * 设置面板的开关与**初始分类**。
   *
   * 分类跟着"从哪进来的"走：菜单进来是「通用」，标题栏右侧的 Node 按钮进来是「Node 版本」。
   * 两个入口打开的是**同一个面板**（不是两套界面渲染同一份状态）。
   */
  const [settings, setSettings] = useState<{ open: boolean; section: SettingsSection }>({
    open: false,
    section: 'general',
  });
  /** 帮助菜单里的弹窗（同时只会有一个） */
  const [helpDialog, setHelpDialog] = useState<HelpDialog | null>(null);
  /**
   * 面板里切换过 Node 版本就 +1，让标题栏按钮上那个版本号跟着变
   * （此刻面板还开着，不能等它关闭再刷）。
   */
  const [nodeRefreshSeq, setNodeRefreshSeq] = useState(0);

  /** 两个菜单共同的容器：点它以外的地方（含点另一个菜单之外的窗口区域）一律收起 */
  const menusRef = useRef<HTMLDivElement>(null);
  // escape：菜单是键盘可达的（Tab 到按钮 + 回车展开），那就得能用 Esc 退出——
  // 只给鼠标用户留出口等于键盘用户展开后关不掉
  useClickOutside(menusRef, () => setOpenMenu(null), { escape: true });

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

  /** 打开设置面板并落在指定分类（两个入口都走这里） */
  const openSettings = useCallback((section: SettingsSection) => {
    setOpenMenu(null);
    setSettings({ open: true, section });
  }, []);

  /** 打开帮助菜单里的某个弹窗 */
  const openHelpDialog = useCallback((which: HelpDialog) => {
    setOpenMenu(null);
    setHelpDialog(which);
  }, []);

  /**
   * 直接开外部网页（菜单项用完即收）。
   *
   * 菜单条目分两类，这是其中一类：**把用户送出去**（报告问题），而不是开自己的弹窗
   * （使用文档 / 关于）。不走 webview 内导航：应用有导航守卫，会被拦成"点了没反应"。
   */
  const openExternal = useCallback((url: string) => {
    setOpenMenu(null);
    void openUrl(url).catch((err) => reportError('打开网页失败', err));
  }, []);

  return (
    <>
    <div className="h-[32px] bg-nexus-titlebar flex items-center select-none flex-shrink-0" onMouseDown={handleMouseDown}>
      {/* ── 左上角：应用级菜单（设置 / 帮助）──
           按 Windows 通例，菜单在左、工具在右。这里**刻意只放固定文字的项**：
           左列宽度一变，中间的项目名就跟着左右漂——`设置`+`帮助` 是两个恒定宽度的词，
           宽度天然固定（这一点在把 Node 版本从右边挪走之后更明显：右侧空出来的宽度
           正好补给了项目名，它现在比之前更接近窗口正中）。 */}
      <div className="relative h-full flex" ref={menusRef} data-menu>
        <TitleMenu
          label="设置"
          open={openMenu === 'settings'}
          onToggle={() => setOpenMenu(openMenu === 'settings' ? null : 'settings')}
        >
          <MenuItem onClick={() => openSettings('general')}>通用</MenuItem>
          <MenuItem onClick={() => openSettings('node')}>Node 版本</MenuItem>
        </TitleMenu>

        <TitleMenu
          label="帮助"
          open={openMenu === 'help'}
          onToggle={() => setOpenMenu(openMenu === 'help' ? null : 'help')}
        >
          <MenuItem onClick={() => openHelpDialog('docs')}>使用文档</MenuItem>
          {/* 直接跳浏览器，不开自己的弹窗：见 utils/links.ts 的 NEW_ISSUE_URL 说明 */}
          <MenuItem onClick={() => openExternal(NEW_ISSUE_URL)}>报告问题</MenuItem>
          {/* 分隔线：上面是文档类入口，下面是"这是哪个应用"——两类东西 */}
          <MenuSeparator />
          <MenuItem onClick={() => openHelpDialog('about')}>关于 Nexus</MenuItem>
          <CheckUpdateItem onDone={() => setOpenMenu(null)} />
        </TitleMenu>
      </div>

      {/* ── 项目名 ── */}
      <div className="flex-1 flex items-center justify-center h-full">
        <span className="text-[12px] text-nexus-text-muted">
          {projectName || ''}
        </span>
      </div>

      {/* ── 右侧：工具 ｜ 窗口控制 ──
           竖线画在这里才有实义：左边是应用自己的功能，右边是系统的窗口按钮。
           （之前它画在「Nexus」和「Node」之间，隔开的是两件同类的东西，等于没分组） */}
      <NodeVersionButton onOpen={() => openSettings('node')} refreshSeq={nodeRefreshSeq} />
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

    {/* 所有浮层都在标题栏的拖拽区**之外**（见 handleMouseDown：它只跳过 button/[data-menu]，
        而这些浮层里大片区域两者都不是——挂在里面就会"点弹窗把窗口拖走"，用户报过） */}
    <SettingsModal
      open={settings.open}
      initialSection={settings.section}
      onClose={() => setSettings(s => ({ ...s, open: false }))}
      onNodeChanged={() => setNodeRefreshSeq(s => s + 1)}
    />
    <UsageDocsModal open={helpDialog === 'docs'} onClose={() => setHelpDialog(null)} />
    <AboutModal open={helpDialog === 'about'} onClose={() => setHelpDialog(null)} />
    </>
  );
}

// ─── 菜单件 ──────────────────────────────────────────

/**
 * 一个顶级菜单：固定文字的按钮 + 它的下拉。
 *
 * `data-menu` 不是可有可无的：标题栏的 mousedown 会 `startDragging()`，只跳过
 * `button, [data-menu]`——少了它，点下拉里的空白处（内边距、分隔线两侧）就会
 * **把整个窗口拖走**（这个坑踩过一次：当初 Node 版本面板挂在拖拽区里面，
 * 点它的背景就把窗口拖走了，见 `TitleBar` 里 handleMouseDown 的说明）。
 */
function TitleMenu({ label, open, onToggle, children }: {
  label: string;
  open: boolean;
  onToggle: () => void;
  children: React.ReactNode;
}) {
  return (
    <div className="relative h-full" data-menu>
      <button
        className={`h-full px-2.5 text-[12.5px] transition-colors ${
          open ? 'bg-nexus-hover text-nexus-text' : 'text-nexus-text-muted hover:text-nexus-text hover:bg-nexus-hover/40'
        }`}
        // 展开状态要让读屏知道（它是"按下去会展开一层"的按钮，不是普通按钮）
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={(e) => { e.stopPropagation(); onToggle(); }}
      >
        {label}
      </button>
      {open && (
        <div
          role="menu"
          /* 键盘：Tab / Shift+Tab 在条目间移动（条目本来就是 button），Esc 收起（见 useClickOutside）。
             完整的 ARIA menu 语义还要求上下方向键与首字母跳转，这里没实现——所以 role 描述的是
             "这是一组命令"，而不是"这是按标准菜单键盘模型操作的控件"。 */
          className="absolute top-full left-0 mt-0.5 min-w-[160px] bg-nexus-surface border border-nexus-border rounded-md shadow-xl py-1 z-50"
        >
          {children}
        </div>
      )}
    </div>
  );
}

/** 下拉里的一个条目 */
function MenuItem({ onClick, children }: { onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      role="menuitem"
      className="w-full px-3 py-1.5 text-[12px] text-nexus-text hover:bg-nexus-accent hover:text-white text-left transition-colors"
      onClick={(e) => { e.stopPropagation(); onClick(); }}
    >
      {children}
    </button>
  );
}

/** 下拉里的分隔线（两侧留一点空隙，别贴着条目） */
function MenuSeparator() {
  return <div role="separator" className="my-1 h-px bg-nexus-border" />;
}

/**
 * 「检查更新」：拉 GitHub 的最新 release，与本机版本比。
 *
 * 版本号问后端要（`CARGO_PKG_VERSION`）而不是读 `package.json`——后者是 npm 侧的，
 * 与这个二进制未必同步；比的是"用户装的这一份"有没有新版，就得用它自己的版本。
 *
 * 三种结果都要说清楚，尤其"比不出来"：把它和"已是最新"混为一谈的话，界面会把一个
 * 解析失败说成结论。
 *
 * 单独成组件是因为它自带两个状态（进行中、本机版本号），而版本号要显示在这一行的右侧。
 */
function CheckUpdateItem({ onDone }: { onDone: () => void }) {
  /** 检查进行中（条目显示"检查中…"并禁用，避免连点发出一串请求） */
  const [checking, setChecking] = useState(false);
  /**
   * 应用自身版本，显示在「检查更新」那一行的右侧。
   *
   * 为什么挂这儿：它是"你装的到底是哪一版"这个问题的答案，而这个问题在报障、对照发布
   * 说明时会被反复问起——挨着更新入口放，问的人和答的人看的是同一处。
   * （「关于 Nexus」里也有版本号，那是"应用信息"的权威落点，两处不是同一个用途。）
   */
  const [appVersion, setAppVersion] = useState('');

  useEffect(() => {
    let alive = true;
    getAppVersion()
      .then(v => { if (alive) setAppVersion(v); })
      .catch(() => { if (alive) setAppVersion(''); });   // 读不到就只显示「检查更新」，不报错
    return () => { alive = false; };
  }, []);

  const handleCheck = async (e: React.MouseEvent) => {
    e.stopPropagation();
    onDone();
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
          // 说清主体：同一个应用里还有 AI 面板的「检查更新」（那是 dsh 引擎），
          // 只写"有新版本"用户会不确定刚检查的是哪个
          title: `Nexus 有新版本 ${latest}`,
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
        notify({ title: 'Nexus 已是最新版本', description: `当前 ${current}`, duration: 2500 });
      }
    } catch (err) {
      // 网络不通 / GitHub 限流 / CSP 没放开——原因都在 err 里，别只说"失败"
      reportError('检查更新失败', err);
    } finally {
      setChecking(false);
    }
  };

  return (
    <button
      role="menuitem"
      className="group w-full flex items-center justify-between gap-3 px-3 py-1.5 text-[12px] text-nexus-text hover:bg-nexus-accent hover:text-white text-left transition-colors disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-nexus-text"
      disabled={checking}
      onClick={handleCheck}
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
  );
}

/**
 * 标题栏右侧的 Node 版本入口（在窗口按钮左边，与它们之间隔一条竖线）。
 *
 * **它与「设置 > Node 版本」是同一个面板的两个入口**，不是两套界面：
 * - 按钮留着，是因为这里能**一眼看到当前版本**（`Node v22.22.1`），而且常见操作
 *   （切版本）只要一次点击——把入口整个搬进菜单，这两件事都要多一步；
 * - 设置里也有它，是因为"Node 版本"在用户心里就是一个设置项，找它的第一反应是去设置里翻。
 * 两边指同一个 `SettingsModal`（只是初始分类不同），所以不存在"两处状态要对齐"的问题。
 *
 * 为什么在这里而不是左上角：左上角是"这个应用能干什么"（菜单），工具排右边是 Windows 上的
 * 通例（Chrome 的扩展图标、VS Code 的布局按钮都在这个位置），
 * 也让左列宽度固定下来——否则项目名会随这里的版本号长短左右漂。
 *
 * 样式上做成圆角小控件（而不是像窗口按钮那样通栏高亮）：它和左上角的菜单是同一层级的
 * 东西，只因为"菜单在左、工具在右"才分居两端；跟系统的三个窗口按钮不是一类。
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
