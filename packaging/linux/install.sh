#!/usr/bin/env bash
# Nexus Linux 服务端安装脚本。
#
# 默认按当前身份选择模式：root 装系统级（服务器推荐），普通用户装用户级（桌面）。
# 系统级可用 --run-as <用户> 在安装当天就写好 systemd unit 并启动 runner。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODE=""
RUN_AS=""

usage() {
  cat <<'EOF'
用法：./install.sh [--system|--user] [--run-as <用户>]

  --system        安装到 /usr/local/bin、/etc/nexus、/var/lib/nexus（需要 root）
  --user          安装到 ~/.local/bin、~/.config/nexus、~/.nexus（桌面场景）
  --run-as <用户> 系统级：runner 以该用户身份运行服务；同时立即安装并启用开机 unit
  -h, --help      显示本帮助

不指定 --system/--user 时：root 用系统级，其它用户用用户级。
服务器（无桌面）只需要系统级：sudo ./install.sh --system --run-as <运行用户>
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --system) MODE="system"; shift ;;
    --user) MODE="user"; shift ;;
    --run-as)
      [ "$#" -ge 2 ] || { echo "[nexus] --run-as 缺少用户名" >&2; exit 2; }
      RUN_AS="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "[nexus] 未知参数: $1" >&2
      usage >&2
      exit 2 ;;
  esac
done

if [ -z "$MODE" ]; then
  if [ "$(id -u)" -eq 0 ]; then MODE="system"; else MODE="user"; fi
fi
if [ "$MODE" = "user" ] && [ -n "$RUN_AS" ]; then
  echo "[nexus] --run-as 只用于系统级；桌面用户级以当前用户运行" >&2
  exit 2
fi

if [ "$MODE" = "system" ]; then
  if [ "$(id -u)" -ne 0 ]; then
    echo "[nexus] --system 需要 root（请用 sudo ./install.sh --system --run-as <用户>）" >&2
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
  echo "[nexus] 已安装：$BIN_DIR/nexusctl、$BIN_DIR/nexus-boot-runner"
  echo "[nexus] 配置：$CONF_FILE（示例：$ROOT/share/nexus.example.toml，不会被复制）"

  if [ -z "$RUN_AS" ]; then
    RUN_AS="${SUDO_USER:-}"
  fi
  if [ -n "$RUN_AS" ]; then
    echo "[nexus] 安装 systemd unit（服务运行用户：$RUN_AS）..."
    "$BIN_DIR/nexusctl" --run-as "$RUN_AS" boot install
    echo "[nexus] 完成。后续命令不需要 --system："
    echo "  nexusctl status"
    echo "  nexusctl service add -p <项目> -n <服务> -c '<命令>' --project-path <项目目录> --boot"
    echo "  systemctl status nexus-boot-runner"
  else
    echo "[nexus] 下一步（root 直接运行时要指定服务用户）："
    echo "  sudo nexusctl --run-as <运行用户> boot install"
    echo "然后：nexusctl status"
  fi
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
  echo "[nexus] 已安装：$BIN_DIR/nexusctl、$BIN_DIR/nexus-boot-runner"
  echo "[nexus] 配置：$CONF_FILE（示例：$ROOT/share/nexus.example.toml，不会被复制）"
  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "[nexus] 注意：$BIN_DIR 不在 PATH 里，请把它加进去" ;;
  esac
  echo "[nexus] 桌面模式下一步："
  echo "  nexusctl --user boot install"
  echo "  sudo loginctl enable-linger \"\$USER\"   # 无人登录也启动"
fi
