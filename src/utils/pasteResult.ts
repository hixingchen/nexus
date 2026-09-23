/**
 * 粘贴结果的**文案规则**（纯函数，不 import 任何东西，可直接跑）。
 *
 * 为什么单独成文件：一次粘贴有五种互不重叠的结局——落地的、失败的、**被跳过的链接**、
 * **被替换掉的**、**用户主动跳过同名的**——而它们此前只有"成功/失败"两句台词，后三种都
 * 说不清。规则写进组件里就只能靠手点验证，而它恰恰分支最多（一个没落地 / 只落了部分 /
 * 重名改了号 / 内容不全 / 替换了旧文件 / 全跳过）。
 *
 * 为什么类型定义在这里而不是 `services/system.ts`：utils 必须零依赖才能被 node 直接跑，
 * 而 `services/system.ts` 引了 `@tauri-apps/api/core`。字段名由后端 `contract.rs` 的
 * `keys_of` 断言钉住，所以这里只声明**一处**，服务层 import 回去用。
 */

/** 与后端 `PasteFilesResult` 同形 */
export interface PasteOutcome {
  /** 已落盘路径（同名冲突时已带 " (2)" 序号） */
  created: string[];
  /** 逐个源的失败原因 */
  failed: string[];
  /** 被跳过的符号链接/联接点：复制本身成功，但这些源不在结果里 */
  skipped: string[];
  /** 被覆盖掉的原项（已移入回收站）——覆盖是唯一会动**已有数据**的那条路 */
  replaced: string[];
  /** 用户在同名弹框里选「跳过」的源（复制没失败，是用户决定不复制） */
  skipped_by_user: string[];
}

/** 与后端 `PasteConflict` 同形：目标目录里已有的一个同名项 */
export interface PasteConflict {
  name: string;
  /** 已存在的那一项是目录：替换它是整棵替换，代价比换掉一个文件大得多 */
  existing_is_dir: boolean;
}

/**
 * 同名冲突的处理策略，取值与后端 `ConflictPolicy` 一一对应。
 * 改这里必须同时改后端（`contract.rs` 的 `test_conflict_policy_wire_values` 钉着这件事）。
 */
export type ConflictPolicy = 'rename' | 'overwrite' | 'skip';

/**
 * 粘贴的两阶段响应，与后端 `PasteResponse` 同形。
 *
 * `status` 是判别式：`conflict` = 有同名、**还没复制任何东西**，要先让用户定策略；
 * `done` = 已执行完，报结果。
 */
export type PasteResponse =
  | { status: 'conflict'; conflicts: PasteConflict[]; sources: string[] }
  | ({ status: 'done' } & PasteOutcome);

/**
 * 粘贴的传输层签名（生产实现是 `services/system` 的 `pasteFiles`）。
 *
 * 单独抽出来是为了让**动作层能被测**：`pasteInto` 的接线错误（第二个参数传没传、源清单
 * 有没有原样带回）类型检查抓不到，只有"发出去的参数是什么"的断言能抓——那需要换掉这一个
 * 出口，而不是换掉整个 `invoke`。
 */
export type PasteTransport = (
  targetDir: string,
  conflictPolicy: ConflictPolicy | null,
  expectedSources: string[] | null,
) => Promise<PasteResponse>;

export interface PasteNotice {
  variant: 'success' | 'warning' | 'error';
  title: string;
  description?: string;
}

/** 名单最多列几个，超出只给个数（粘 500 个文件时把名字全铺出来没有意义） */
const MAX_LISTED = 5;

/** 取路径末段（树里的路径是 `/` 与 `\` 混用的，两种都要认） */
function baseName(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

/** 名字列表：短则全列，长则列前几个 + 总数 */
function listNames(paths: string[]): string {
  const names = paths.map(baseName);
  if (names.length <= MAX_LISTED) return names.join('、');
  return `${names.slice(0, MAX_LISTED).join('、')} 等 ${names.length} 个`;
}

/**
 * 把一次粘贴的结果翻成一条提示。
 *
 * 三条口径：
 * - **落地名要说出来**。重名会自动改号成 " (2)"，用户不看名字就不知道新文件叫什么
 *   （单个项目时名字直接进标题，省掉一次重复）。
 * - **跳过 ≠ 成功**。被跳过的链接让复制结果**内容不全**，必须单独说，不能并进任何一边。
 * - **失败最显眼**（error 档、停留 8s）：部分成功时标题同时给出两个数。
 *
 * 判档位的分界不是"有没有动过已有的东西"，而是**这件事是不是用户自己选的**：
 * 替换和"跳过同名"都是他在弹框里点下来的，照办就不该报成警告；被跳过的链接他没选过、
 * 也不知情，那才是警告（复制结果内容不全）——换了判据的话，用户每次选覆盖都会挨一句黄字。
 * 唯一的例外是**整批什么都没落地**：那时净效果为零，绿字会被读成"成功了"。
 */
export function describePaste(res: PasteOutcome): PasteNotice {
  const { created, failed, skipped, replaced, skipped_by_user } = res;
  const linkNote = `${skipped.length} 个符号链接/联接点被跳过（复制结果里没有它们）`;

  const parts: string[] = [];
  // 单个项目时名字已进标题，再列一遍是重复
  if (created.length > 1) parts.push(`落地：${listNames(created)}`);
  if (replaced.length > 0) {
    parts.push(`${replaced.length} 个同名项被替换（原项已移入回收站）：${listNames(replaced)}`);
  }
  if (skipped_by_user.length > 0) {
    parts.push(`按你的选择跳过 ${skipped_by_user.length} 个同名项：${listNames(skipped_by_user)}`);
  }
  if (skipped.length > 0) parts.push(`${linkNote}：${listNames(skipped)}`);
  // 一个都没落地时标题已经说完了结局，再加"失败："是重复
  if (failed.length > 0) parts.push(created.length === 0 ? failed.join('；') : `失败：${failed.join('；')}`);

  let title: string;
  if (created.length === 0) {
    // 什么都没落地时，标题要说出"为什么"——三种原因各是一句话
    if (failed.length > 0) title = `粘贴失败（${failed.length} 个）`;
    else if (skipped_by_user.length > 0) title = `已跳过 ${skipped_by_user.length} 个同名项`;
    // 只剩 skipped：剪贴板里就是链接，被有意跳过了。老老实实说"什么都没复制"，
    // 而不是让调用方去说"已粘贴 0 个项目"
    else title = '没有复制任何内容';
  } else if (failed.length > 0) {
    title = `已粘贴 ${created.length} 个，${failed.length} 个失败`;
  } else {
    title = created.length === 1 ? `已粘贴「${baseName(created[0])}」` : `已粘贴 ${created.length} 个项目`;
  }

  const variant = failed.length > 0
    ? 'error'
    : skipped.length > 0 || created.length === 0
      ? 'warning'
      : 'success';
  return parts.length > 0 ? { variant, title, description: parts.join('；') } : { variant, title };
}
