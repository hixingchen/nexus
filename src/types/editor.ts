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
}
