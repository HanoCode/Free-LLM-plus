#!/usr/bin/env bash
# Freeport 构建打包（release）：产出 .app 与 .dmg 到 target/release/bundle
# 用法: ./sbin/build.sh [tauri build 参数，如 --debug]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
[ -f "$APP_DIR/package.json" ] || APP_DIR="$APP_DIR/app"

info() { printf '\033[36m[freeport]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[freeport]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31m[freeport]\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "仅支持 macOS"
command -v node >/dev/null 2>&1 || die "缺少 node（建议 nvm 安装 LTS）"
command -v cargo >/dev/null 2>&1 || die "缺少 cargo（https://rustup.rs）"
[ -d "$APP_DIR/node_modules" ] || die "缺少 node_modules，请先执行: cd \"$APP_DIR\" && npm ci"
[ -x "$APP_DIR/node_modules/.bin/tauri" ] || die "缺少 tauri CLI，请先执行: cd \"$APP_DIR\" && npm ci"

cd "$APP_DIR"
BUNDLE_DIR="$APP_DIR/target/release/bundle"
info "APP_DIR = $APP_DIR"
info "开始构建（前端 tsc+vite，随后 cargo release 编译，首次较慢）"
START=$SECONDS

FAILED=0
if [ $# -gt 0 ]; then
  npm run build -- "$@" || FAILED=1
else
  npm run build || FAILED=1
fi

shopt -s nullglob
APPS=("$BUNDLE_DIR"/macos/*.app)
DMGS=("$BUNDLE_DIR"/dmg/*.dmg)

if [ "$FAILED" = "1" ]; then
  if [ ${#APPS[@]} -gt 0 ]; then
    warn "打包未完成，但 .app 已产出（dmg 阶段失败多为 hdiutil 权限问题，可在本机终端重试）"
    printf '  %s\n' "${APPS[0]}" >&2
  fi
  exit 1
fi

info "构建完成，用时 $((SECONDS - START))s"
printf '\n产物:\n'
for p in ${APPS[@]+"${APPS[@]}"}; do printf '  %s  (%s)\n' "$p" "$(du -sh "$p" | cut -f1 | tr -d ' ')"; done
for p in ${DMGS[@]+"${DMGS[@]}"}; do printf '  %s  (%s)\n' "$p" "$(du -sh "$p" | cut -f1 | tr -d ' ')"; done
