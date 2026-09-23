import { Modal } from '../ui/Modal';
import { usePasteConflictStore } from '../../stores/pasteConflict';

/**
 * 粘贴同名冲突弹框：**整批一次决策**（保留两者 / 覆盖 / 跳过 / 取消）。
 *
 * 为什么整批一次，而不是像资源管理器那样逐个文件问：策略对**不冲突的那些源毫无影响**
 * （它们照常复制），逐个问只会把同一个问题问 N 遍。要逐个决定的是"目录怎么合并"，
 * 那是另一个量级的功能——后端只判顶层名字，选覆盖就是**整棵替换**，所以下面那一句
 * 提示在撞到文件夹时必须说出来。
 *
 * 为什么每行写成"动作 + 后果"而不是并排三个按钮：三个选项的差别全在后果上
 * （一个留旧文件、一个移走旧文件、一个不复制），挤成按钮就只剩"覆盖"两个字可读，
 * 而它恰恰是唯一不可逆的那个。
 */
export function PasteConflictModal() {
  const pending = usePasteConflictStore(s => s.pending);
  const settle = usePasteConflictStore(s => s.settle);

  if (!pending) return null;
  const { conflicts } = pending;
  const dirCount = conflicts.filter(c => c.existing_is_dir).length;

  return (
    <Modal open title="粘贴同名项目" onClose={() => settle(null)} width="460px">
      <div className="space-y-3">
        <p className="text-[13px] text-nexus-text">
          目标文件夹中已有 <span className="text-nexus-warning font-medium">{conflicts.length}</span> 个同名项目：
        </p>

        {/* 名单可滚动而不是截断：用户要据此决定"覆盖哪些"，把名字藏起来就没法判断 */}
        <div className="max-h-[132px] overflow-y-auto rounded border border-nexus-border bg-nexus-bg/30">
          {conflicts.map((c, i) => (
            <div
              key={c.name}
              className={`flex items-center gap-2 px-2.5 py-1 text-[12px] ${
                i > 0 ? 'border-t border-nexus-border/40' : ''
              }`}
            >
              <span className="flex-1 truncate text-nexus-text" title={c.name}>{c.name}</span>
              <span className="flex-shrink-0 text-[10px] text-nexus-muted">
                {c.existing_is_dir ? '文件夹' : '文件'}
              </span>
            </div>
          ))}
        </div>

        {dirCount > 0 && (
          <p className="text-[11.5px] text-nexus-warning">
            其中 {dirCount} 个是文件夹：选「覆盖」会整棵替换，不会逐个文件比对合并。
          </p>
        )}

        <div className="space-y-1">
          <Choice
            label="保留两者"
            hint="新文件复制为「xxx (2).txt」，原有文件一个都不动"
            onClick={() => settle('rename')}
          />
          <Choice
            label="覆盖"
            hint="同名项先移入系统回收站，新文件落到原名"
            onClick={() => settle('overwrite')}
          />
          <Choice
            label="跳过"
            hint="这些同名的不复制，其余照常"
            onClick={() => settle('skip')}
          />
        </div>

        <div className="flex items-center justify-end">
          <button
            className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
            onClick={() => settle(null)}
          >取消（都不复制）</button>
        </div>
      </div>
    </Modal>
  );
}

function Choice({ label, hint, onClick }: { label: string; hint: string; onClick: () => void }) {
  return (
    <button
      className="flex w-full items-baseline gap-3 rounded border border-nexus-border px-3 py-2 text-left transition-colors hover:border-nexus-accent/50 hover:bg-nexus-accent/10"
      onClick={onClick}
    >
      <span className="w-[56px] flex-shrink-0 text-[12.5px] text-nexus-text">{label}</span>
      <span className="text-[11px] text-nexus-muted">{hint}</span>
    </button>
  );
}
