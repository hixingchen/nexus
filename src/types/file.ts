/** 文件/目录条目（来自 Rust 后端 list_directory） */
export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  extension: string | null;
}
