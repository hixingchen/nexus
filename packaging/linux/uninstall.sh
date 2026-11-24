#!/usr/bin/env bash
# 卸载 Nexus Linux 服务端二进制与 systemd unit；默认保留配置/日志。
set -euo pipefail

MODE=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --system|--user) MODE="${1#--}"; shift ;;
    -h|--help)
      echo "用法：./uninstall.sh [--system|--user]"
      echo "不指定时：root 按系统级，其它用户按用户级。"
      exit 0 ;;
    *)
      echo "[nexus] 未知参数: $1" >&2
      exit 2 ;;
  esac
done
if [ -z "$MODE" ]; then
  if [ "$(id -u)" -eq 0 ]; then MODE="system"; else MODE="user"; fi
fi

if [ "$MODE" = "system" ]; then
  if [ "$(id -u)" -ne 0 ]; then
    echo "[nexus] --system 需要 root" >&2
    exit 1
  fi
  /usr/local/bin/nexusctl boot uninstall || true
  rm -f /usr/local/bin/nexusctl /usr/local/bin/nexus-boot-runner
  echo "[nexus] 已删除二进制与 unit；配置 /etc/nexus、数据 /var/lib/nexus 保留"
else
  CTL="${HOME}/.local/bin/nexusctl"
  [ -x "$CTL" ] || CTL="nexusctl"
  "$CTL" --user boot uninstall || true
  rm -f "${HOME}/.local/bin/nexusctl" "${HOME}/.local/bin/nexus-boot-runner"
  echo "[nexus] 已删除二进制与 unit；配置 ~/.config/nexus、数据 ~/.nexus 保留"
fi
