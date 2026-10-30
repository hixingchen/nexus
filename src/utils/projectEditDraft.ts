/**
 * 「编辑项目」弹窗是否可以把当前表单写进草稿。
 *
 * 背景（实际 bug）：弹窗组件常驻、只切换 `project` prop。用户先编辑项目 A、关闭，
 * 再右键编辑项目 B 时，`name/path` 还停在上一次渲染的 A 值；草稿 effect 先于
 * “把 B 种进表单”的 effect 执行，会把 A 的值写进 B 的草稿键，随后 B 的初始化又从
 * 草稿里把这几个值读回来——表现为“编辑 B 却显示 A 的信息”。
 *
 * 判据：**表单已经为当前项目种过值**才允许同步草稿。切换项目后的第一帧
 * `seededId` 仍是上一个项目（或 null），必须跳过；等 seed effect 完成、重新渲染后
 * 再写。dirty 只决定写内容还是清草稿，不参与“能不能写”的判断。
 */
export function shouldSyncProjectDraft(
  editingProjectId: string | null,
  seededProjectId: string | null,
): boolean {
  return !!editingProjectId && seededProjectId === editingProjectId;
}
