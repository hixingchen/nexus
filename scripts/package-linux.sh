#!/usr/bin/env bash
# 构建 Linux 服务端 tar.gz（在 Linux 上运行；CI 使用 ubuntu-22.04）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ "$(uname -s)" != "Linux" ]; then
  echo "[package-linux] 只能在 Linux 上运行（当前 $(uname -s)）。Linux 包不能从 Windows 交叉产出。" >&2
  exit 1
fi

VERSION="${VERSION:-$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$ROOT/src-tauri/linux/Cargo.toml" | head -n 1)}"
if [ -z "$VERSION" ]; then
  echo "[package-linux] 无法从 src-tauri/linux/Cargo.toml 解析版本号" >&2
  exit 1
fi

case "$(uname -m)" in
  x86_64|amd64) DEFAULT_ARCH="x86_64" ;;
  aarch64|arm64) DEFAULT_ARCH="aarch64" ;;
  *) DEFAULT_ARCH="$(uname -m)" ;;
esac
ARCH="${ARCH:-$DEFAULT_ARCH}"
STAGE_NAME="nexus-${VERSION}-linux-${ARCH}"
OUT_DIR="$ROOT/src-tauri/target/linux-package"
STAGE="$OUT_DIR/$STAGE_NAME"

echo "[package-linux] building nexus-linux $VERSION ($ARCH)"
(cd "$ROOT/src-tauri" && cargo build --release -p nexus-linux --bin nexusctl --bin nexus-boot-runner)

rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/share"

cp "$ROOT/src-tauri/target/release/nexusctl" "$STAGE/bin/nexusctl"
cp "$ROOT/src-tauri/target/release/nexus-boot-runner" "$STAGE/bin/nexus-boot-runner"
chmod +x "$STAGE/bin/nexusctl" "$STAGE/bin/nexus-boot-runner"

# strip 失败不影响打包（有些构建环境没有 strip）
strip "$STAGE/bin/nexusctl" "$STAGE/bin/nexus-boot-runner" 2>/dev/null || true

cp "$ROOT/packaging/linux/nexus.example.toml" "$STAGE/share/nexus.example.toml"
cp "$ROOT/packaging/linux/README.md" "$STAGE/share/README.md"
cp "$ROOT/packaging/linux/install.sh" "$STAGE/install.sh"
cp "$ROOT/packaging/linux/uninstall.sh" "$STAGE/uninstall.sh"
chmod +x "$STAGE/install.sh" "$STAGE/uninstall.sh"

(cd "$STAGE" && sha256sum bin/nexusctl bin/nexus-boot-runner > SHA256SUMS)

tar -C "$OUT_DIR" -czf "$OUT_DIR/$STAGE_NAME.tar.gz" "$STAGE_NAME"
(cd "$OUT_DIR" && sha256sum "$STAGE_NAME.tar.gz" > "$STAGE_NAME.tar.gz.sha256")

echo "[package-linux] artifact: $OUT_DIR/$STAGE_NAME.tar.gz"
echo "[package-linux] checksum: $OUT_DIR/$STAGE_NAME.tar.gz.sha256"
