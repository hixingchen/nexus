/**
 * Nexus 会话级 dsh 插件：AI 回合结束时把本轮改过的文件上报给 Nexus。
 *
 * Nexus 用 `--patch` 把本文件挂进 dsh 会话，配置为：
 *   { url: 'http://127.0.0.1:<port>/ai/edited', token, cwd }
 *
 * 为什么在回合结束而不是每个工具调用后：一轮里改多个文件只重启一次，构建/启动
 * 在回合结束后进行。只收集写文件工具；bash/pwsh 改文件是盲区，由模型显式调用
 * MCP 的 restart_services_after_edit 补报。
 */

import path from 'node:path';

export const name = 'nexus-ai-restart';

const MAX_FILES = 1000;

export function apply(ctx, config) {
  const url = String(config?.url || '');
  const token = String(config?.token || '');
  const cwd = String(config?.cwd || '').replace(/\\/g, '/').replace(/\/+$/, '');
  if (!url || !token) return;

  const warn = (message) => {
    try {
      if (typeof ctx?.logger?.warn === 'function') ctx.logger.warn(`[nexus-ai-restart] ${message}`);
      else console.warn(`[nexus-ai-restart] ${message}`);
    } catch {
      /* 日志失败不能影响回合 */
    }
  };

  /** @type {Map<string, { files: Set<string>, handled: Set<string> | null }>} */
  const buffers = new Map();

  const agentKey = (agent) => String(agent?.id || agent?.session?.id || 'agent');

  const bufferFor = (agent) => {
    const key = agentKey(agent);
    let buffer = buffers.get(key);
    if (!buffer) {
      buffer = { files: new Set(), handled: null };
      buffers.set(key, buffer);
    }
    return buffer;
  };

  /** MCP 工具名是 `mcp__nexus__xxx`，原生工具是裸名；统一取最后一段 */
  const baseToolName = (name) => {
    const raw = String(name || '');
    const marker = raw.lastIndexOf('__');
    return marker >= 0 ? raw.slice(marker + 2) : raw;
  };

  const resolvePath = (raw) => {
    const value = String(raw || '').trim();
    if (!value) return '';
    const normalized = value.replace(/\\/g, '/');
    if (/^[A-Za-z]:\//.test(normalized) || normalized.startsWith('//') || normalized.startsWith('/')) {
      return normalized;
    }
    if (!cwd) return '';
    // apply_patch 的路径相对 Session 工作目录，可能带 `..`；用 node:path 归一化，
    // 否则 Nexus 侧的前缀匹配会漏掉跨目录改动。
    return path.resolve(cwd, normalized).replace(/\\/g, '/');
  };

  const addFile = (buffer, raw) => {
    const path = resolvePath(raw);
    if (path && buffer.files.size < MAX_FILES) buffer.files.add(path);
  };

  const patchPaths = (input) => {
    if (typeof input !== 'string') return [];
    const paths = [];
    for (const line of input.split(/\r?\n/)) {
      const match = /^\*\*\*\s+(Add File|Update File|Delete File|Move to):\s*(.+?)\s*$/.exec(line);
      if (match) paths.push(match[2]);
    }
    return paths;
  };

  const collect = (exec, result) => {
    try {
      const tool = baseToolName(exec?.name);
      if (!tool) return;
      const buffer = bufferFor(exec?.agent);

      if (tool === 'restart_services_after_edit') {
        // 模型自己调了工具且成功：只把**它实际传入的文件**记为已处理。
        // 回合结束补发其余文件（模型可能只传了子集，或者之后又改了几个）。
        if (!result?.isError) {
          const raw = exec?.arguments?.changed_files ?? exec?.arguments?.changedFiles;
          const handled = new Set();
          if (Array.isArray(raw)) {
            for (const value of raw) {
              const path = resolvePath(value);
              if (path) handled.add(path);
            }
          }
          buffer.handled = handled;
        }
        return;
      }
      if (result?.isError) return;

      const args = exec?.arguments || {};
      const value = result?.value || {};
      if (tool === 'write' || tool === 'edit') {
        addFile(buffer, value.path || args.file_path || args.path);
      } else if (tool === 'str_replace_editor') {
        if (args.command !== 'view') addFile(buffer, value.path || args.path);
      } else if (tool === 'apply_patch') {
        for (const path of patchPaths(args.input)) addFile(buffer, path);
      }
    } catch (error) {
      warn(`收集改动失败: ${error?.message || error}`);
    }
  };

  const flush = (payload) => {
    try {
      const key = agentKey(payload?.agent);
      const buffer = buffers.get(key);
      if (!buffer) return;
      buffers.delete(key);
      const handled = buffer.handled;
      const files = [...buffer.files].filter((path) => !handled || !handled.has(path));
      if (files.length === 0) return;

      const init = {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          authorization: `Bearer ${token}`,
        },
        body: JSON.stringify({ changed_files: files.slice(0, MAX_FILES), source: 'dsh-turn' }),
      };
      try {
        init.signal = AbortSignal.timeout(8000);
      } catch {
        /* 老 Node 没有 AbortSignal.timeout */
      }
      // 不 await：回合关闭不能被"停服务 + 启动 + 构建"的耗时拖住；失败只告警
      void fetch(url, init)
        .then((response) => {
          if (!response.ok) warn(`Nexus 返回 ${response.status}`);
        })
        .catch((error) => warn(`上报改动失败: ${error?.message || error}`));
    } catch (error) {
      warn(`回合结束处理失败: ${error?.message || error}`);
    }
  };

  try {
    ctx.on('tools/result', collect);
    ctx.on('agent/turn-stopping', flush);
  } catch (error) {
    warn(`注册事件失败（插件将不生效）: ${error?.message || error}`);
  }
}
