import { useEffect, useState } from 'react';
import { Modal } from '../ui/Modal';
import { GeneralSection } from './GeneralSection';
import { EditorSection } from './EditorSection';
import { NodeVersionSection } from './NodeVersionSection';
import { AiRuntimeSection } from './AiRuntimeSection';

/**
 * 设置面板：**全部应用设置的唯一容器**（左分类 + 右内容）。
 *
 * 为什么是一个面板而不是"每个设置项一个小弹窗"：设置项只会增加（通用、Node 版本，
 * 之后还有别的），一项一个弹窗的话，每一项都要自带一套"标题栏 + 关闭按钮 + 宽度"，
 * 加第三项时就会开始互相不一致（宽度/内边距/滚动行为各写各的）。面板只有一个外壳，
 * 加分类 = 往左栏加一行。
 *
 * 为什么外壳直接用 `Modal` 的 `sidebar` 变体而不是自己写一个：焦点陷阱、Esc 只关栈顶、
 * `pushModal` 让 AiPanel 把原生子 WebView 移出屏幕——这三件事都在 `Modal` 里，
 * 而它们各自都有失败史（见 `ui/Modal.tsx` 的注释）。复制一份壳就等于把它们复制一遍。
 *
 * **两个入口共用本组件**（见 `TitleBar`）：标题栏右侧的「Node v22.22.1」按钮与
 * 菜单里的「设置 > Node 版本」打开的是同一个面板、同一个分类——不是两套界面渲染同一份状态。
 */
export type SettingsSection = 'general' | 'editor' | 'node' | 'ai';

/** 左栏分类表（顺序即显示顺序） */
const SECTIONS: { key: SettingsSection; label: string }[] = [
  { key: 'general', label: '通用' },
  { key: 'editor', label: '编辑器' },
  { key: 'node', label: 'Node 版本' },
  { key: 'ai', label: 'AI 运行时' },
];

interface SettingsModalProps {
  open: boolean;
  /** 打开时落在哪个分类：菜单进来是 `general`，标题栏的 Node 按钮进来是 `node` */
  initialSection?: SettingsSection;
  onClose: () => void;
  /** 面板里切换过 Node 版本后调用（让标题栏按钮上的版本号当场更新，见 NodeVersionSection） */
  onNodeChanged?: () => void;
}

export function SettingsModal({
  open,
  initialSection = 'general',
  onClose,
  onNodeChanged,
}: SettingsModalProps) {
  const [section, setSection] = useState<SettingsSection>(initialSection);

  /**
   * 每次打开都回到入口指定的分类。
   *
   * 不重置的话会这样：上次停在「Node 版本」，这次从菜单点「设置 > 通用」，
   * 面板却打开在 Node 版本那一页——用户只会以为自己点错了，或者以为菜单坏了。
   */
  useEffect(() => {
    if (open) setSection(initialSection);
  }, [open, initialSection]);

  const sidebar = (
    <nav aria-label="设置分类" className="flex flex-col">
      {SECTIONS.map(({ key, label }) => {
        const active = section === key;
        return (
          <button
            key={key}
            type="button"
            // aria-current 而不是只有颜色：左栏的选中态只靠背景色的话，读屏用户
            // 听不出自己在哪一页（与窄轨项目列同一套口径）
            aria-current={active ? 'page' : undefined}
            className={`w-full text-left px-3 py-1.5 text-[12px] transition-colors ${
              active
                ? 'bg-nexus-accent/15 text-nexus-accent'
                : 'text-nexus-text-muted hover:text-nexus-text hover:bg-nexus-hover/40'
            }`}
            onClick={() => setSection(key)}
          >
            {label}
          </button>
        );
      })}
    </nav>
  );

  return (
    <Modal
      open={open}
      title="设置"
      onClose={onClose}
      width="720px"
      sidebar={sidebar}
      /**
       * 固定外框高度（内容随分类长短变化，但弹窗本身不跳）。
       *
       * 用 `clamp` 而不是写死像素：写死 700px 会在小窗口（本项目 minHeight 600）里把面板
       * 顶出屏幕，底部内容永远够不着；而这个表达式对**内容**是固定值，只随窗口大小变。
       */
      height="clamp(440px, calc(100vh - 120px), 700px)"
    >
      {/* 只挂载当前分类：Node 版本那一屏会在挂载时联网拉版本索引（几百 KB），
          两个分类都常驻就等于每次打开设置都拉一次——而多数时候用户是来改布局的 */}
      {section === 'general' && <GeneralSection />}
      {section === 'editor' && <EditorSection />}
      {section === 'node' && <NodeVersionSection onChanged={onNodeChanged} />}
      {section === 'ai' && <AiRuntimeSection />}
    </Modal>
  );
}
