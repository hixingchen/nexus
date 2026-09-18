import { useCallback, useEffect, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useEditorStore, saveAllDirtyTabs } from '../../stores/editor';
import { useServiceDraftStore } from '../../stores/serviceDraftStore';
import { prepareExit } from '../../services/system';
import { reportError } from '../../utils/error';
import { Modal } from '../ui/Modal';
import { showNotification } from '../ui/Toast';

/**
 * 关窗守卫：把**所有**关闭窗口的入口（标题栏 ✕ / Alt+F4 / 任务栏右键关闭）收敛到一条路径。
 *
 * 背景：未保存草稿只活在内存（`stores/editor.ts` 的模块级 `drafts`），原先标题栏 ✕ 直接
 * `appWindow.close()` → 进程退出 → 草稿与未保存标签**无声消失**；而标签级关闭却是有确认的，
 * 同一应用两套语义，用户会误以为草稿安全。
 *
 * 时序（与后端 `commands/app.rs::prepare_exit` 配套）：
 * 1. 关闭请求 → Tauri 检测到前端有 `tauri://close-requested` 监听 → 自动 `prevent_close()`
 *    并把决定权交给这里（`lib.rs` 的 `WindowEvent::CloseRequested` 因此不再做清理）；
 * 2. 有未保存草稿 → 弹确认（保存全部 / 放弃更改 / 取消）；取消即什么都不做，窗口继续存活；
 * 3. 确认关闭 → 调 `prepare_exit`：**独立线程**里停服务/停 AI/停监听，完成后 `app.exit(0)`。
 *    清理不在事件线程上跑，服务多时窗口也不会整段"未响应"（原实现在 CloseRequested 里同步做）。
 */
export function CloseGuard() {
  const fileDirtyCount = useEditorStore(s => s.dirtyIds.length);
  // 服务/模板编辑面板里的改动同样只活在内存（草稿按 id 存在 store 里，进程退出即消失）
  const formDirtyCount = useServiceDraftStore(s => Object.keys(s.drafts).length);
  const dirtyCount = fileDirtyCount + formDirtyCount;
  const [askClose, setAskClose] = useState(false);
  const [exiting, setExiting] = useState(false);
  const [saving, setSaving] = useState(false);
  /** 监听回调只注册一次，闭包里的 dirtyCount 会过期——用 ref 读最新值 */
  const dirtyRef = useRef(dirtyCount);
  dirtyRef.current = dirtyCount;

  const beginExit = useCallback(async () => {
    setAskClose(false);
    setExiting(true);
    try {
      await prepareExit();
      // 正常情况下不会走到这里：后端清理完成即退出进程。走到这里说明命令已受理但进程未退，
      // 保留遮罩继续等待（不要再放开界面，避免用户在半清理状态下继续操作）。
    } catch (e) {
      setExiting(false);
      reportError('退出失败', e);
    }
  }, []);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    let disposed = false;
    let unlisten: (() => void) | null = null;
    appWindow
      .onCloseRequested(event => {
        // 一律拦下：由这里决定"弹确认"还是"直接退出"，否则窗口会立刻销毁（草稿丢失）
        event.preventDefault();
        if (dirtyRef.current > 0) setAskClose(true);
        else void beginExit();
      })
      .then(fn => { if (disposed) fn(); else unlisten = fn; })
      .catch(e => console.error('订阅窗口关闭请求失败（未保存确认将失效）:', e));
    return () => { disposed = true; unlisten?.(); };
  }, [beginExit]);

  // 浏览器级加速键：WebView2 默认仍然响应刷新与关窗，一次习惯性的 Ctrl+R/F5 就会把
  // 内存里的草稿全部冲掉（代码里已经专门吞掉 Ctrl+F 阻止原生查找框，这里是同一类缺口）。
  // 有未保存草稿时拦下刷新并说明原因；Ctrl+W 与标题栏 ✕ 走同一条确认路径。
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      const isReload = e.key === 'F5' || (mod && (e.key === 'r' || e.key === 'R'));
      const isCloseWin = mod && (e.key === 'w' || e.key === 'W');
      if (isCloseWin) {
        e.preventDefault();
        getCurrentWindow().close().catch(err => console.error('请求关闭窗口失败:', err));
        return;
      }
      if (!isReload) return;
      const dirty = dirtyRef.current;
      if (dirty === 0) return; // 无草稿：刷新无损失，照旧放行（开发期调试也不受影响）
      e.preventDefault();
      showNotification({
        variant: 'warning',
        title: `已拦截刷新：还有 ${dirty} 个文件未保存`,
        description: '刷新会清空内存中的草稿。请先保存（Ctrl+S），或直接关闭窗口走确认流程',
        duration: 6000,
      });
    };
    document.addEventListener('keydown', onKeyDown, true);
    return () => document.removeEventListener('keydown', onKeyDown, true);
  }, []);

  const handleSaveAllAndExit = async () => {
    setSaving(true);
    const { saved, failed } = await saveAllDirtyTabs();
    setSaving(false);
    if (failed === 0) {
      void beginExit();
      return;
    }
    // 有文件没保存成功（冲突/未加载/只读）：不退出，列出数量让用户决定后续
    showNotification({
      variant: 'error',
      title: `还有 ${failed} 个文件未能保存`,
      description: `已保存 ${saved} 个。请处理冲突后重试，或选择「放弃更改并关闭」`,
      duration: 8000,
    });
  };

  return (
    <>
      <Modal open={askClose && !exiting} title="有未保存的更改" onClose={() => setAskClose(false)}>
        {fileDirtyCount > 0 && (
          <p className="text-[12.5px] text-nexus-text leading-relaxed">
            还有 <span className="text-nexus-warning font-semibold">{fileDirtyCount}</span> 个文件未保存。
            直接关闭会<span className="text-nexus-error font-semibold">永久丢失</span>这些修改
            （草稿只存在内存里，没有恢复入口）。
          </p>
        )}
        {formDirtyCount > 0 && (
          <p className={`text-[12.5px] text-nexus-text leading-relaxed ${fileDirtyCount > 0 ? 'mt-2' : ''}`}>
            另有 <span className="text-nexus-warning font-semibold">{formDirtyCount}</span> 个服务/模板的配置改动未保存
            （在服务编辑面板里，需先点「保存配置」）。
          </p>
        )}
        <div className="mt-4 flex items-center justify-end gap-2">
          <button
            className="px-3 py-1.5 text-[12px] text-nexus-text bg-nexus-bg border border-nexus-border rounded hover:bg-nexus-hover/50 disabled:opacity-40"
            disabled={saving}
            onClick={() => setAskClose(false)}
          >取消</button>
          <button
            className="px-3 py-1.5 text-[12px] text-nexus-error border border-nexus-error/40 rounded hover:bg-nexus-error/10 disabled:opacity-40"
            disabled={saving}
            onClick={() => void beginExit()}
          >放弃更改并关闭</button>
          {fileDirtyCount > 0 && (
            <button
              className="px-3 py-1.5 text-[12px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover disabled:opacity-40"
              disabled={saving}
              onClick={() => void handleSaveAllAndExit()}
            >{saving ? '正在保存…' : `保存 ${fileDirtyCount} 个文件并关闭`}</button>
          )}
        </div>
      </Modal>

      {/* 退出遮罩：后端在独立线程里停服务/停 AI/停监听（秒级），期间必须拦住继续操作，
          否则用户会在"正在被清理"的会话上继续点按钮 */}
      <Modal open={exiting} title="正在退出" onClose={() => { /* 退出中不可取消 */ }}>
        <div className="flex items-center gap-3 py-1">
          <div className="w-4 h-4 border-2 border-nexus-accent/30 border-t-nexus-accent rounded-full animate-spin" />
          <p className="text-[12.5px] text-nexus-text">正在停止服务与会话，请稍候…</p>
        </div>
      </Modal>
    </>
  );
}
