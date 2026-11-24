#!/usr/bin/env bash
# Nexus Linux 服务端安装脚本：只放二进制与示例配置，不自动启用 systemd。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODE="${1:---user}"

if [ "$MODE" = "--system" ]; then
  if [ "$(id -u)" -ne 0 ]; then
    echo "[nexus] --system 需要 root" >&2
    exit 1
  fi
  BIN_DIR="/usr/local/bin"
  CONF_DIR="/etc/nexus"
  CONF_FILE="$CONF_DIR/nexus.toml"
  install -d -m 0755 "$BIN_DIR" "$CONF_DIR"
  install -m 0755 "$ROOT/bin/nexusctl" "$BIN_DIR/nexusctl"
  install -m 0755 "$ROOT/bin/nexus-boot-runner" "$BIN_DIR/nexus-boot-runner"
  if [ ! -f "$CONF_FILE" ]; then
    # 正式配置从空开始，不复制示例项目/服务，避免装完就出现 app/api
    printf 'version = 1\n' > "$CONF_FILE"
    chmod 0600 "$CONF_FILE"
  fi
  echo "[nexus] 示例配置（不会被复制到正式配置）：$ROOT/share/nexus.example.toml"
  echo "[nexus] 已安装到 $BIN_DIR"
  echo "[nexus] 配置：$CONF_FILE"
  echo "[nexus] 下一步：sudo $BIN_DIR/nexusctl --system --run-as <用户> boot install"
else
  BIN_DIR="${HOME}/.local/bin"
  CONF_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/nexus"
  CONF_FILE="$CONF_DIR/nexus.toml"
  install -d -m 0755 "$BIN_DIR" "$CONF_DIR"
  install -m 0755 "$ROOT/bin/nexusctl" "$BIN_DIR/nexusctl"
  install -m 0755 "$ROOT/bin/nexus-boot-runner" "$BIN_DIR/nexus-boot-runner"
  if [ ! -f "$CONF_FILE" ]; then
    # 正式配置从空开始，不复制示例项目/服务，避免装完就出现 app/api
    printf 'version = 1\n' > "$CONF_FILE"
    chmod 0600 "$CONF_FILE"
  fi
  echo "[nexus] 示例配置（不会被复制到正式配置）：$ROOT/share/nexus.example.toml"
  echo "[nexus] 已安装到 $BIN_DIR"
  echo "[nexus] 配置：$CONF_FILE"
  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "[nexus] 注意：$BIN_DIR 不在 PATH 里，请把它加进去" ;;
  esac
  echo "[nexus] 下一步：编辑 $CONF_FILE，或执行 nexusctl project add / nexusctl service add"
  echo "[nexus] 然后：nexusctl boot install"
fi
