// ── 工作目录变化后，「监听路径」该取什么值 ──────────────────────────
//
// 为什么必须重置：服务/模板的监听路径是跟着工作目录配的，而从模板添加服务会把
// 模板里的 watch_paths 一起复制过来（模板作者机器上的目录，或相对旧目录的写法）。
// 用户随后改工作目录时，这两个字段隔着几屏，很容易只改 cwd、忘了改监听路径——
// 结果监听盯着一个与当前工作目录无关的位置，改代码不会触发重启，而且界面上
// 看不出任何异常。工作目录一变，旧路径几乎必然失效，默认就应当回到"跟随新目录"。
//
// 唯一例外是**用户在本面板里显式编辑过监听路径**：那是明确的意图（多根监听、
// 排除某目录等），不该被一次目录改动静默覆盖掉。
//
// 纯函数、零依赖，可直接被 Node 内置测试器跑（与 utils/projectDetail 同一套做法）。

/**
 * @param touched   本面板里用户是否手动编辑过监听路径
 * @param nextCwd   新的工作目录（空串 = 还没填）
 * @returns 新的监听路径文本；`null` = 保持原值不动
 */
export function watchPathsAfterCwdChange(params: { touched: boolean; nextCwd: string }): string | null {
  if (params.touched) return null;
  // 与后端同一口径：单个路径用 JSON 数组表示；空串时给 '[]'（后端再回落到 cwd）
  return params.nextCwd ? JSON.stringify([params.nextCwd]) : '[]';
}
