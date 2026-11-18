import { useEffect, useLayoutEffect, useState, type MouseEvent } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { listen } from '@tauri-apps/api/event';
import { noticeClicked, noticeReady } from '../services/system';
import { reportError } from '../utils/error';

/**
 * 自制通知小窗（后端 `nexus-notice` 窗口的根组件）。
 *
 * 与主界面共用同一份前端 bundle，由 `main.tsx` 根据初始化脚本写入的
 * `window.__NEXUS_NOTICE__` 分流；窗口本身由后端创建（无边框、置顶、跳过任务栏、
 * 不抢焦点），这里只负责画卡片与点击行为：
 * - 点卡片 → `notice_clicked`（后端显示主窗口并关闭小窗）；
 * - 点 ✕ → 只关小窗；
 * - 渲染完成后 `notice_ready` 再显示窗口，避免白底闪一下；
 * - 后续通知通过 `nexus-notice` 事件更新同一条卡片，不叠窗口。
 *
 * 不用 Windows toast 的原因：WinRT 需要注册 AppUserModelID，dev/免安装版可能弹不出来；
 * 这个窗口由应用自己画，dev 与打包版表现一致。
 */
export interface NoticePayload {
  title: string;
  body: string;
  /** 后端白名单归一，这里只用于选图标/颜色；未知值按 info 处理 */
  level: string;
}

/** 自适应尺寸测量用的根节点 id（只在本组件内使用） */
const NOTICE_ROOT_ID = 'nexus-notice-root';

declare global {
  interface Window {
    /** 由 Rust 的初始化脚本写入（仅通知窗存在） */
    __NEXUS_NOTICE__?: NoticePayload;
  }
}

const LEVEL_STYLES: Record<string, { icon: string; color: string; bg: string }> = {
  error: { icon: '✕', color: 'text-nexus-error', bg: 'bg-nexus-error/10' },
  warning: { icon: '⚡', color: 'text-nexus-warning', bg: 'bg-nexus-warning/10' },
  success: { icon: '✓', color: 'text-emerald-400', bg: 'bg-emerald-400/10' },
  info: { icon: 'ℹ', color: 'text-nexus-info', bg: 'bg-nexus-info/10' },
};

export function NoticeWindow({ initial }: { initial: NoticePayload }) {
  const [notice, setNotice] = useState(initial);

  // 按内容自适应窗口：卡片宽度取 fit-content（上限由 max-w 控制），这里量出
  // 自然宽高交给后端调整窗口；短通知不再留一大片空白。
  useLayoutEffect(() => {
    const root = document.getElementById(NOTICE_ROOT_ID);
    const card = root?.firstElementChild as HTMLElement | null;
    if (!root || !card) return;
    // 当前窗口可能比这条通知窄（上一条是短文本），先把卡片临时设为 max-content
    // 再量；不改这一下，短→长的更新会把旧宽度当成上限，窗口永远长不回来。
    const prevWidth = card.style.width;
    card.style.width = 'max-content';
    const rect = card.getBoundingClientRect();
    const rootStyle = getComputedStyle(root);
    const padX = parseFloat(rootStyle.paddingLeft) + parseFloat(rootStyle.paddingRight);
    const padY = parseFloat(rootStyle.paddingTop) + parseFloat(rootStyle.paddingBottom);
    // 在绘制前还原，避免 max-content 影响这一帧的实际布局
    card.style.width = prevWidth;
    void noticeReady(Math.ceil(rect.width + padX), Math.ceil(rect.height + padY)).catch((e) =>
      reportError('调整通知窗大小失败', e, { silent: true }),
    );
  }, [notice.title, notice.body, notice.level]);

  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | null = null;
    void listen<NoticePayload>('nexus-notice', (event) => {
      if (alive) setNotice(event.payload);
    })
      .then((fn) => {
        if (alive) unlisten = fn;
        else fn();
      })
      .catch((e) => reportError('订阅通知更新失败', e, { silent: true }));

    // 失去焦点（用户切回主窗口/别的应用）就收起：置顶窗口不该长期压着界面。
    // 主窗口获得焦点的路径后端也会关一次，这里是双保险。
    const win = getCurrentWindow();
    let unlistenFocus: (() => void) | null = null;
    let hasFocused = false;
    void win.onFocusChanged(({ payload: focused }) => {
      if (focused) {
        hasFocused = true;
        return;
      }
      // 新窗口是 focused(false) 创建的，可能先收到一次 false——只在真正获得过
      // 焦点之后失去焦点时才关闭，避免通知一出现就自己关掉
      if (hasFocused) {
        void win.close().catch(() => {
          // 已经关了/正在关：忽略
        });
      }
    }).then((fn) => {
      if (alive) unlistenFocus = fn;
      else fn();
    }).catch((e) => reportError('订阅通知焦点失败', e, { silent: true }));

    return () => {
      alive = false;
      unlisten?.();
      unlistenFocus?.();
    };
  }, []);

  // 成功/提示类不打扰太久；错误/警告保留到点击或关闭（用户离开时不会错过）
  useEffect(() => {
    if (notice.level === 'error' || notice.level === 'warning') return;
    const timer = setTimeout(() => {
      void getCurrentWindow().close().catch(() => {
        // 关窗失败不影响任何业务；最坏结果是多挂几秒
      });
    }, 6000);
    return () => clearTimeout(timer);
  }, [notice.level, notice.title, notice.body]);

  const style = LEVEL_STYLES[notice.level] ?? LEVEL_STYLES.info;

  const openMain = () => {
    // 命令成功时后端会顺手关掉本窗口，invoke 的响应可能因此拿不到——
    // 这不是失败，别往日志里写一条误导性的"打开主窗口失败"。
    void noticeClicked().catch((e) => {
      console.warn('[nexus] 通知点击命令未返回（小窗可能已关闭）:', e);
    });
  };

  const close = (e: MouseEvent) => {
    e.stopPropagation();
    // 关窗权限在 capabilities 的 core:window:allow-close，所有窗口都适用
    void getCurrentWindow().close().catch((e) => {
      console.warn('[nexus] 关闭通知小窗失败:', e);
    });
  };

  return (
    <div id={NOTICE_ROOT_ID} className="w-fit min-w-[260px] p-1.5 bg-transparent select-none">
      <div
        role="button"
        tabIndex={0}
        title="点击回到 Nexus"
        onClick={openMain}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            openMain();
          }
        }}
        className="flex items-start gap-3 rounded-xl border border-nexus-border bg-nexus-surface max-w-[328px] px-4 py-3 cursor-pointer hover:bg-nexus-surface/95 focus:outline-none focus:border-nexus-accent"
      >
        <div className={`w-7 h-7 rounded-lg ${style.bg} flex items-center justify-center flex-shrink-0 mt-0.5`}>
          <span className={`${style.color} text-sm font-bold`}>{style.icon}</span>
        </div>

        <div className="flex-1 min-w-0">
          <p className="text-[13px] font-semibold text-nexus-text leading-snug line-clamp-2 break-words">
            {notice.title}
          </p>
          {notice.body && (
            <p className="text-[11px] text-nexus-muted mt-0.5 leading-relaxed line-clamp-2 break-words">
              {notice.body}
            </p>
          )}
        </div>

        <button
          aria-label="关闭通知"
          className="p-1 -mr-1 -mt-0.5 text-nexus-muted/50 hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
          onClick={close}
        >
          <svg width="12" height="12" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5">
            <line x1="2" y1="2" x2="8" y2="8" />
            <line x1="8" y1="2" x2="2" y2="8" />
          </svg>
        </button>
      </div>
    </div>
  );
}
