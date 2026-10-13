/** 标签的查看器类型：text（编辑器）/ image（图片预览）/ hex（十六进制）/ jar（jar 包浏览） */
type FileViewerType = 'text' | 'image' | 'hex' | 'jar';

/** 打开的文件标签 */
export interface FileTab {
  id: string;
  name: string;
  path: string;
  /** 只读（>10MB 大文件、.class、图片、hex、编码无法还原的文件均只可查看） */
  readonly?: boolean;
  /** 只读原因（用于保存按钮的悬停提示；缺省时退回通用文案） */
  readonlyReason?: string;
  /** 查看器类型（默认 text） */
  viewerType?: FileViewerType;
  /**
   * Markdown 视图模式：`true` = 渲染预览、`false` = 源码。
   *
   * `undefined`（刚打开的标签）= 用默认值，见 `stores/editor.ts` 的 `MD_PREVIEW_BY_DEFAULT`。
   * 按标签记而不是全局一份：用户把某个文件切成源码，不该连带改掉其它文件的默认视图。
   */
  preview?: boolean;
}
