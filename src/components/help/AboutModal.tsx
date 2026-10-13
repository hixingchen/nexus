import { useEffect, useState } from 'react';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { Modal } from '../ui/Modal';
import { getAppVersion } from '../../services/system';
import { reportError } from '../../utils/error';
import { REPO_URL, repoBlobUrl } from '../../utils/links';

/**
 * 「关于 Nexus」。
 *
 * 为什么版本号要问后端（`getAppVersion`）而不是读 `package.json`：那是 npm 侧的版本号，
 * 与用户装的这个二进制未必同步——"关于"要回答的是"**我装的这一份**是哪个版本"，
 * 报障时对方照着它去对发布说明，读错了等于给出一条误导线索（详见 services/system.ts）。
 *
 * 为什么把第三方组件也写在这里：许可证要求随分发附上声明，而"关于"是用户唯一会去找它的地方。
 */

/**
 * CFR 的许可证原文（随仓库分发，不在安装包里）。
 *
 * 为什么用相对路径 + `repoBlobUrl` 而不是直接拼 GitHub 地址：仓库名与默认分支只在
 * `utils/links.ts` 里有一份（换仓库/改分支时不用回来改这里）。
 */
const CFR_LICENSE_PATH = 'src-tauri/resources/cfr-LICENSE.txt';

export function AboutModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  /**
   * 版本号（空 = 还没读到或读失败，界面显示占位符）。
   *
   * 为什么打开时才读：这个弹窗挂在标题栏上，组件一直在场，而多数用户整个会话都不会点开它
   * ——没必要在启动时白打一次 IPC。
   */
  const [version, setVersion] = useState('');

  /**
   * 读版本：**读不到只显示占位符，不弹错误提示**（与标题栏「帮助 > 检查更新」那一行同一口径）。
   *
   * 为什么不开错误提示：这是"顺带看一眼"的信息性字段，失败时既没有可行动作、也不影响任何
   * 功能，为它弹一条红字提示只会让用户以为出了问题。
   *
   * `alive` 是防"卸载后 setState"：请求在飞的时候弹窗可能已经被关掉，
   * 而 `open` 变回 true 时会重新读一次。
   */
  useEffect(() => {
    if (!open) return;
    let alive = true;
    getAppVersion()
      .then(v => { if (alive) setVersion(v); })
      .catch(() => { if (alive) setVersion(''); });
    return () => { alive = false; };
  }, [open]);

  const openRepo = () => { void openUrl(REPO_URL).catch((err) => reportError('打开网页失败', err)); };
  const openCfrLicense = () => {
    void openUrl(repoBlobUrl(CFR_LICENSE_PATH)).catch((err) => reportError('打开网页失败', err));
  };

  return (
    <Modal open={open} title="关于 Nexus" onClose={onClose} width="420px">
      <div className="space-y-3">
        <div className="flex items-baseline gap-2">
          <span className="text-[15px] font-medium text-nexus-text">Nexus</span>
          {/* `—` 而不是"读取中…"：读不到和还没读到在用户眼里是同一件事（都不确定），
              而缺失的版本号不该被伪装成一个会自己变好的加载态 */}
          <span className="text-[12px] text-nexus-muted">{version ? `v${version}` : '—'}</span>
        </div>

        <p className="text-[12px] text-nexus-muted leading-relaxed">
          开发环境管理平台：把多个项目的多个服务集中在一处启停、看日志、改配置、翻代码。
        </p>

        <div className="flex items-center justify-between border-t border-nexus-border pt-3 text-[12px]">
          <span className="text-nexus-muted">许可证</span>
          <span className="text-nexus-text">MIT</span>
        </div>

        <div className="border-t border-nexus-border pt-3">
          <div className="mb-1.5 text-[12px] text-nexus-text">第三方组件</div>
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0 text-[12px]">
              <div className="text-nexus-text">CFR（MIT License）</div>
              {/* 说清"为什么要带上它"：jar 随安装包分发，用户看不到也不该被要求去猜 */}
              <div className="mt-0.5 text-[11px] text-nexus-muted leading-relaxed">
                Java 字节码反编译；jar 随应用内嵌分发，反编译时不需要联网
              </div>
            </div>
            <button
              className="flex-shrink-0 px-2 py-1 text-[11px] text-nexus-accent border border-nexus-accent/40 rounded hover:bg-nexus-accent/10"
              title="在系统浏览器中查看 CFR 的许可证原文"
              onClick={openCfrLicense}
            >查看许可证</button>
          </div>
        </div>

        <div className="flex justify-end border-t border-nexus-border pt-3">
          <button
            className="px-3 py-1.5 text-[12px] text-nexus-text border border-nexus-border rounded hover:border-nexus-accent hover:text-nexus-accent transition-colors"
            onClick={openRepo}
          >打开仓库</button>
        </div>
      </div>
    </Modal>
  );
}
