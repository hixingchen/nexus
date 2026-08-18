/** 标签的查看器类型：text（编辑器）/ image（图片预览）/ hex（十六进制）/ jar（jar 包浏览） */
export type FileViewerType = 'text' | 'image' | 'hex' | 'jar';

/** 打开的文件标签 */
export interface FileTab {
  id: string;
  name: string;
  path: string;
  /** 只读（>10MB 大文件、.class、图片、hex 均只可查看） */
  readonly?: boolean;
  /** 查看器类型（默认 text） */
  viewerType?: FileViewerType;
}
