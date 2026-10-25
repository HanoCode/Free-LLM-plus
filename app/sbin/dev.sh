#!/usr/bin/env bash
# Freeport 开发模式：前端 Vite(5273) + Rust 网关，热重载
# 用法: ./sbin/dev.sh [tauri dev 参数，如 --no-watch]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
[ -f "$APP_DIR/package.json" ] || APP_DIR="$APP_DIR/app"

info() { printf '\033[36m[freeport]\033[0m %s\n' "$*"; }
die() { printf '\033[31m[freeport]\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "仅支持 macOS"
command -v node >/dev/null 2>&1 || die "缺少 node（建议 nvm 安装 LTS）"
command -v cargo >/dev/null 2>&1 || die "缺少 cargo（https://rustup.rs）"
[ -d "$APP_DIR/node_modules" ] || die "缺少 node_modules，请先执行: cd \"$APP_DIR\" && npm ci"
[ -x "$APP_DIR/node_modules/.bin/tauri" ] || die "缺少 tauri CLI，请先执行: cd \"$APP_DIR\" && npm ci"

cd "$APP_DIR"


# devUrl 固定 5273（见 src-tauri/tauri.conf.json），被占会导致窗口空白
if lsof -nP -iTCP:5273 -sTCP:LISTEN >/dev/null 2>&1; then
  die "端口 5273 已被占用，可能已有 dev 实例在运行。查看: lsof -nP -iTCP:5273 -sTCP:LISTEN"
fi

info "APP_DIR = $APP_DIR"
info "启动 tauri dev（前端 http://127.0.0.1:5273，Ctrl+C 退出）"

if [ $# -gt 0 ]; then
  exec npm run dev -- "$@"
else
  exec npm run dev
fi
