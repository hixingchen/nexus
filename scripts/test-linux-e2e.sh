#!/usr/bin/env bash
# Linux 服务端端到端冒烟：真实 runner + 真实进程组 + 文件命令协议。
# 不依赖 systemd（unit 语法由 CI 的 systemd-analyze verify 单独检查）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ "$(uname -s)" != "Linux" ]; then
  echo "[linux-e2e] 只能在 Linux 上运行（当前 $(uname -s)）" >&2
  exit 1
fi

(cd "$ROOT/src-tauri" && cargo build -q -p nexus-linux --bins)
BIN="$ROOT/src-tauri/target/debug"
TMP="$(mktemp -d)"
RUNNER=""

cleanup() {
  if [ -n "$RUNNER" ] && kill -0 "$RUNNER" 2>/dev/null; then
    "${CTL[@]}" service stop app bootsvc >/dev/null 2>&1 || true
    "${CTL[@]}" service stop app ondemand >/dev/null 2>&1 || true
    "${CTL[@]}" boot shutdown >/dev/null 2>&1 || true
    sleep 0.5
    kill "$RUNNER" 2>/dev/null || true
    wait "$RUNNER" 2>/dev/null || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT

mkdir -p "$TMP/app"
CTL=("$BIN/nexusctl" --config "$TMP/nexus.toml" --data-dir "$TMP/data")

"${CTL[@]}" project add "$TMP/app" --name app >/dev/null
# 开机服务：应自动启动到 ready
"${CTL[@]}" service add -p app -n bootsvc -c "echo bootsvc-started; sleep 30" --boot --order 1 >/dev/null
# 非开机服务：纳入 runner 管理，但启动时只应为 stopped
"${CTL[@]}" service add -p app -n ondemand -c "sleep 30" --order 2 >/dev/null
"${CTL[@]}" boot sync >/dev/null

"$BIN/nexus-boot-runner" \
  --manifest "$TMP/data/boot/systemd/manifest.json" --mode user >/dev/null 2>&1 &
RUNNER=$!

STATE="$TMP/data/boot/systemd/state.json"
for _ in $(seq 1 50); do [ -f "$STATE" ] && break; sleep 0.1; done
for _ in $(seq 1 50); do
  grep -q '"phase": "ready"' "$STATE" 2>/dev/null && break
  sleep 0.1
done
grep -q '"phase": "ready"' "$STATE" || {
  echo "[linux-e2e] 开机服务没有进入 ready：" >&2
  cat "$STATE" >&2 || true
  exit 1
}
grep -q '"phase": "stopped"' "$STATE" || {
  echo "[linux-e2e] 非开机服务不应被自动拉起：" >&2
  cat "$STATE" >&2
  exit 1
}

echo "=== 详情/配置/日志 ==="
"${CTL[@]}" project show app
"${CTL[@]}" service show app bootsvc
"${CTL[@]}" config path
"${CTL[@]}" config validate
"${CTL[@]}" service logs app bootsvc --lines 20 | grep -q 'bootsvc-started'
"${CTL[@]}" status
"${CTL[@]}" status --json | grep -q '"unit"'

echo "=== 停止语义：setsid 后代 + 宽限 ==="
"${CTL[@]}" service add -p app -n setsid -c "setsid sleep 300 & wait" --order 4 --stop-timeout 2 >/dev/null
"${CTL[@]}" start app setsid
"${CTL[@]}" service show app setsid | grep -q '停止宽限:   2s'
"${CTL[@]}" stop app setsid
if pgrep -f "sleep 300" >/dev/null 2>&1; then
  echo "[linux-e2e] setsid 子进程未被停止：" >&2
  pgrep -af "sleep 300" >&2 || true
  exit 1
fi
"${CTL[@]}" service remove app setsid >/dev/null

# double-fork：父进程先退出，setsid 子进程靠实例环境标记找回
"${CTL[@]}" service add -p app -n daemon -c "setsid sh -c 'sleep 300' >/dev/null 2>&1 &" --order 5 --stop-timeout 2 >/dev/null
"${CTL[@]}" start app daemon
sleep 0.6
"${CTL[@]}" service show app daemon | grep -q '运行状态:   ready'
"${CTL[@]}" stop app daemon
if pgrep -f "sleep 300" >/dev/null 2>&1; then
  echo "[linux-e2e] double-fork 子进程未被停止：" >&2
  pgrep -af "sleep 300" >&2 || true
  exit 1
fi
"${CTL[@]}" service remove app daemon >/dev/null

"${CTL[@]}" service add -p app -n term -c "trap '' TERM; sleep 30" --order 6 --stop-timeout 1 >/dev/null
"${CTL[@]}" start app term
"${CTL[@]}" stop app term
"${CTL[@]}" service remove app term >/dev/null

echo "=== 新增 --boot 服务自动启动 ==="
"${CTL[@]}" service add -p app -n autoboot -c "echo autoboot-started; sleep 30" --boot --order 3
"${CTL[@]}" service show app autoboot | grep -q '运行状态:   ready'
"${CTL[@]}" logs app autoboot --lines 20 | grep -q 'autoboot-started'

echo "=== 快捷启停 ==="
"${CTL[@]}" stop app autoboot
"${CTL[@]}" start app autoboot

echo "=== 项目启停 ==="
"${CTL[@]}" project start app
"${CTL[@]}" service list
"${CTL[@]}" project stop app
"${CTL[@]}" service list
"${CTL[@]}" project start app
"${CTL[@]}" service list

"${CTL[@]}" boot shutdown >/dev/null
for _ in $(seq 1 50); do
  kill -0 "$RUNNER" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "$RUNNER" 2>/dev/null; then
  echo "[linux-e2e] runner 在 shutdown 后仍存活" >&2
  exit 1
fi
echo "[linux-e2e] ok"
