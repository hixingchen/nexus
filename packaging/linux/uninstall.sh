#!/usr/bin/env bash
# 卸载 Nexus Linux 服务端二进制与 systemd unit；默认保留配置/日志。
set -euo pipefail

MODE="${1:---user}"

if [ "$MODE" = "--system" ]; then
  if [ "$(id -u)" -ne 0 ]; then
    echo "[nexus] --system 需要 root" >&2
    exit 1
  fi
  /usr/local/bin/nexusctl --system boot uninstall || true
  rm -f /usr/local/bin/nexusctl /usr/local/bin/nexus-boot-runner
  echo "[nexus] 已删除二进制与 unit；配置 /etc/nexus、数据 /var/lib/nexus 保留"
else
  CTL="${HOME}/.local/bin/nexusctl"
  [ -x "$CTL" ] || CTL="nexusctl"
  "$CTL" boot uninstall || true
  rm -f "${HOME}/.local/bin/nexusctl" "${HOME}/.local/bin/nexus-boot-runner"
  echo "[nexus] 已删除二进制与 unit；配置 ~/.config/nexus、数据 ~/.nexus 保留"
fi
