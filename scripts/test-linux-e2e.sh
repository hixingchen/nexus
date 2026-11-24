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
"${CTL[@]}" service add -p app -n bootsvc -c "sleep 30" --boot --order 1 >/dev/null
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

"${CTL[@]}" service start app ondemand >/dev/null
"${CTL[@]}" service stop app bootsvc >/dev/null
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
