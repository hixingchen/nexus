import { pasteFiles } from '../services/system';
import { notify } from '../utils/notify';
import { reportError } from '../utils/error';
import { describePaste, type PasteTransport } from '../utils/pasteResult';
import { usePasteConflictStore } from './pasteConflict';

/**
 * 粘贴系统剪贴板文件到目标目录，随后刷新——服务启停那套**共享动作层**的同款
 * （见 `stores/serviceActions.ts` 对"为什么放 store 层"的说明）。
 *
 * 为什么从 `FileTree.tsx` 里挪出来：三个入口（目录右键 / 文件右键的"粘贴到同级" /
 * 空白区右键到项目根）共用它，而 **.tsx 里的逻辑进不了 node 测试**
 * （JSX 过不了 Node 的类型擦除，见 `test/ts-hooks.mjs`）。本轮加的两阶段流程恰恰是
 * "发出去的参数对不对"最要紧的一段：策略传没传、源清单有没有原样带回——这类接线错误
 * 类型检查与手点都拦不住（见 `__tests__/pasteActions.test.ts`）。
 *
 * `setBusy` 由调用方给：粘贴可能很慢（几百 MB 的目录），而它此前**没有任何进行中反馈**
 * ——点了"粘贴到此处"之后界面几十秒不动，看起来像没反应。
 */
export async function pasteInto(
  targetDir: string,
  refresh: () => void,
  setBusy: (busy: boolean) => void,
  /** 传输层。**只该由测试传**，生产一律用 `services/system` 的实现 */
  paste: PasteTransport = pasteFiles,
): Promise<void> {
  setBusy(true);
  try {
    // 先探测（后端**一个字节都不落盘**）。没有同名就与加这个功能之前完全一样，
    // 直接粘完走人——**常规粘贴不能多一步弹框**
    const probe = await paste(targetDir, null, null);
    if (probe.status === 'conflict') {
      // 取消（null）= 一个都不复制。磁盘上没有任何变化，正是他要的，所以不提示
      const policy = await usePasteConflictStore.getState().ask(probe);
      if (!policy) return;
      // 带策略再调一次。源清单**原样回传**，后端拿它跟当前剪贴板比对
      // （弹框开着时用户完全可能去别处又复制了一次）
      const done = await paste(targetDir, policy, probe.sources);
      // 带了策略后端不会再要求决策。真发生说明策略没落地，**必须说出来**：
      // 静默 return 的话，用户点了「覆盖」却什么都没发生、也没有任何报错
      if (done.status !== 'done') {
        reportError('粘贴未执行', '后端仍要求处理同名冲突（策略未生效）');
        return;
      }
      notify(describePaste(done));
    } else {
      notify(describePaste(probe));
    }
    refresh();
  } catch (err) {
    reportError('粘贴失败', err);
  } finally {
    setBusy(false);
  }
}
