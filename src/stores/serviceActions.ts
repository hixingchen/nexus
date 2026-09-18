import { processApi, watchApi, type Service } from '../services/service';
import { useLogStore } from './logStore';
import { useRunningStore } from './runningStore';
import { showNotification } from '../components/ui/Toast';
import { reportError } from '../utils/error';

/**
 * 服务启停的**共享动作层**（ARCH-7 ①）。
 *
 * 背景：同一套"启动项目"流程此前有两个实现——左侧项目列表（`useProjectList.handleStart`）
 * 与右侧详情页（`useProjectDetail.handleStartAll`）。两处都要：启动/停止全部 → 汇总逐服务
 * 失败清单 → 启动/停止项目级文件监听 → 刷新运行状态 → 清日志（仅停止时）。改一处忘另一处
 * 就成了"左边启动会开监听、右边不会"这类只在特定入口复现的错。
 *
 * 为什么放 store 层而不是抽个 hook：调用方是**事件处理器**（按钮点击），不是渲染期逻辑，
 * 不需要订阅/状态；写成纯函数可以同时被 hook、右键菜单、快捷键复用，也不引入新的订阅点。
 */

/** 启动项目：返回逐服务失败清单（空数组 = 全部成功） */
export async function startProjectServices(projectId: string): Promise<string[]> {
  const errors = await processApi.startProject(projectId);
  // 启动项目级文件监听（所有 restart_mode>0 且启用的服务）；监听失败不影响"启动成功"结论，
  // 但要留痕——自动重启是这套工具的主要卖点，静默失败会表现为"改了代码没反应"
  watchApi.start(projectId).catch((e) => reportError('启动文件监听失败', e, { silent: true }));
  await useRunningStore.getState().refresh();
  return errors;
}

/** 停止项目：返回逐服务失败清单；停止后清空本项目所有服务日志（"全部停止"= 主动关闭） */
export async function stopProjectServices(projectId: string, services: Service[]): Promise<string[]> {
  // 后端 stop_project_services 同时停进程与项目级监听（总开关），并回传逐服务失败清单
  const errors = await processApi.stopProject(projectId);
  // 全部停止即"本次运行结束"：日志一并清空（含返回失败的服务——它们下次启动会重新输出，
  // 而旧日志留着只会让下一次排查看到两轮混在一起的输出）
  for (const s of services) {
    useLogStore.getState().clearLogs(s.id);
  }
  await useRunningStore.getState().refresh();
  return errors;
}

/** 启动项目并把"部分失败"如实告诉用户（调用方只关心是否需要刷新界面） */
export async function startProjectWithFeedback(
  projectId: string,
  projectName: string,
): Promise<void> {
  try {
    const errors = await startProjectServices(projectId);
    if (errors.length > 0) {
      showNotification({
        variant: 'error',
        title: `「${projectName}」部分服务启动失败`,
        description: errors.join(', '),
      });
    } else {
      showNotification({ title: `「${projectName}」已启动`, description: '所有已启用的服务已启动' });
    }
  } catch (e) {
    reportError(`启动「${projectName}」失败`, e);
  }
}

/** 停止项目并把"部分失败"如实告诉用户 */
export async function stopProjectWithFeedback(
  projectId: string,
  projectName: string,
  services: Service[],
): Promise<void> {
  try {
    const errors = await stopProjectServices(projectId, services);
    if (errors.length > 0) {
      showNotification({
        variant: 'error',
        title: `「${projectName}」部分服务停止失败`,
        description: errors.join(', '),
      });
    } else {
      showNotification({ variant: 'info', title: `「${projectName}」已停止`, description: '所有服务已停止' });
    }
  } catch (e) {
    reportError(`停止「${projectName}」失败`, e);
  }
}

/**
 * 启动单个服务（服务卡片按钮/右键菜单）。
 *
 * 单服务启动时同步启动该服务的文件监听（与"项目级监听"是两条路：单服务启动不会启动
 * 项目级监听，否则会顺带监听该项目下其他服务）。
 */
export async function startSingleService(projectId: string, serviceId: string): Promise<void> {
  await processApi.start(serviceId);
  watchApi.start(projectId, serviceId).catch((e) => reportError('启动文件监听失败', e, { silent: true }));
}

/** 停止单个服务：连同它的文件监听一起停（否则改了代码还会触发重启已停的服务） */
export async function stopSingleService(projectId: string, serviceId: string): Promise<void> {
  await processApi.stop(serviceId);
  watchApi.stop(projectId, serviceId).catch((e) => reportError('停止文件监听失败', e, { silent: true }));
  useLogStore.getState().clearLogs(serviceId);
}
