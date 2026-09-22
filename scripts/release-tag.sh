#!/usr/bin/env bash
#
# 发布 tag：更新版本号 → 跑质量门禁 → 提交 → 打 tag → 推送。
#
# 推送 tag 会自动触发 .github/workflows/release.yml：各平台打包 +
# 创建 GitHub Release（产物下载与 release notes 都在那里）。
#
# 用法：
#   ./scripts/release-tag.sh                # 用 Cargo.toml 里现有版本号打 tag
#   ./scripts/release-tag.sh patch         # 0.1.0 → 0.1.1
#   ./scripts/release-tag.sh minor         # 0.1.0 → 0.2.0
#   ./scripts/release-tag.sh major         # 0.1.0 → 1.0.0
#   ./scripts/release-tag.sh 1.2.3         # 显式指定
#   ./scripts/release-tag.sh patch --no-push   # 只本地打 tag，稍后自己推
#   ./scripts/release-tag.sh patch --skip-checks  # 跳过 fmt/clippy/test，快速发布
#
# 质量门禁与最终确认都是方向键菜单（↑/↓ 移动光标，回车确认，默认选中
# 第一项）；选「跳过」直接发布，push 后 CI 仍会兜底跑一遍。--skip-checks
# 免交互直接跳过门禁，适合赶时间的快速发布。
#
# 唯一 versions 真源：[workspace.package].version（各 crate 都是
# version.workspace = true），所以只改这一处。

set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

# ---------------------------------------------------------------- 参数解析
BUMP=""
NO_PUSH=0
SKIP_CHECKS=0

usage() {
  sed -n '3,22p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

for arg in "$@"; do
  case "$arg" in
    -h | --help) usage 0 ;;
    --no-push) NO_PUSH=1 ;;
    --skip-checks) SKIP_CHECKS=1 ;;
    patch | minor | major) BUMP="$arg" ;;
    *) BUMP="$arg" ;;
  esac
done

# ---------------------------------------------------------------- 前置检查
die() { printf '\033[31m✗ %s\033[0m\n' "$1" >&2; exit 1; }
step() { printf '\033[36m→ %s\033[0m\n' "$1"; }

# 方向键菜单：↑/↓ 移动光标，回车确认，Ctrl+C 退出；结果存入全局
# MENU_INDEX（0 起，默认选中第一项）。用法：
#   menu "提示文字" "选项一" "选项二"
#   if [ "$MENU_INDEX" -eq 1 ]; then ...; fi
menu() {
  local prompt="$1"
  shift
  local -a options=("$@")
  local count=$# sel=0 i key rest

  printf '%s\n' "$prompt"
  for i in "${!options[@]}"; do
    if [ "$i" -eq "$sel" ]; then
      printf '\033[36m❯ %s\033[0m\n' "${options[$i]}"
    else
      printf '  %s\n' "${options[$i]}"
    fi
  done

  while :; do
    IFS= read -rsn1 key
    case "$key" in
      $'\033')
        read -rsn2 rest
        case "$rest" in
          '[A' | 'OA')
            if [ "$sel" -gt 0 ]; then
              sel=$((sel - 1))
            fi
            ;;
          '[B' | 'OB')
            if [ "$sel" -lt $((count - 1)) ]; then
              sel=$((sel + 1))
            fi
            ;;
          *) continue 2 ;;
        esac
        printf '\033[%dA' "$count"
        for i in "${!options[@]}"; do
          if [ "$i" -eq "$sel" ]; then
            printf '\033[K\033[36m❯ %s\033[0m\n' "${options[$i]}"
          else
            printf '\033[K  %s\n' "${options[$i]}"
          fi
        done
        ;;
      '' | $'\r') break ;;
      *) ;;
    esac
  done
  MENU_INDEX=$sel
}

command -v git >/dev/null || die "需要 git"
command -v cargo >/dev/null || die "需要 cargo"

git rev-parse --git-dir >/dev/null 2>&1 || die "当前不在 git 仓库里"

[ -z "$(git status --porcelain)" ] || die \
  "工作区有未提交改动，请先提交或 stash（git status 查看详情）"

REMOTE="$(git remote | grep -m1 '^origin$' || true)"
if [ -z "$REMOTE" ]; then
  REMOTE="$(git remote | head -1 || true)"
fi
[ -n "$REMOTE" ] || die \
  "没有配置 git remote，先加一个：git remote add origin <你的 GitHub 仓库 URL>"

BRANCH="$(git rev-parse --abbrev-ref HEAD)"

current_version() {
  sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1
}

CURRENT="$(current_version)"
[ -n "$CURRENT" ] || die "无法从 Cargo.toml 读取 [workspace.package].version"

if [ -n "$BUMP" ]; then
  case "$BUMP" in
    patch | minor | major)
      IFS='.' read -r MA MI PA <<<"$CURRENT"
      case "$BUMP" in
        patch) PA=$((PA + 1)) ;;
        minor) MI=$((MI + 1)); PA=0 ;;
        major) MA=$((MA + 1)); MI=0; PA=0 ;;
      esac
      VERSION="$MA.$MI.$PA"
      ;;
    *)
      if [[ "$BUMP" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        VERSION="$BUMP"
      else
        die "无法识别的版本参数：${BUMP}（可用 patch / minor / major / x.y.z）"
      fi
      ;;
  esac
else
  VERSION="$CURRENT"
fi

TAG="v$VERSION"

git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && die "tag $TAG 已存在"

# ---------------------------------------------------------------- 版本写入
# 先在**改文件之前**跑门禁：不合格时工作区仍是干净的，不必手动回滚。
if [ "$SKIP_CHECKS" -eq 0 ]; then
  menu "质量门禁 fmt / clippy / test 可能需要几分钟" \
    "运行质量门禁（推荐）" \
    "跳过，快速发布（push 后 CI 兜底）"
  if [ "$MENU_INDEX" -eq 1 ]; then
    SKIP_CHECKS=1
    printf '\033[33m! 已跳过质量门禁；push 后 CI 仍会跑一遍，留意 Actions 结果\033[0m\n'
  fi
fi

if [ "$SKIP_CHECKS" -eq 0 ]; then
  step "质量门禁：fmt / clippy / test"
  cargo fmt --all -- --check
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all-features
fi

# 写版本前备份，脚本中途失败自动还原（避免留下一份半改的 Cargo.toml）。
BACKUP_DIR="$(mktemp -d)"
cp Cargo.toml "$BACKUP_DIR/Cargo.toml"
[ -f Cargo.lock ] && cp Cargo.lock "$BACKUP_DIR/Cargo.lock"
NEEDS_RESTORE=0
restore_files() {
  if [ "${NEEDS_RESTORE:-0}" -eq 1 ]; then
    printf '\033[33m↩ 中途失败，已还原 Cargo.toml / Cargo.lock\033[0m\n' >&2
    cp "$BACKUP_DIR/Cargo.toml" Cargo.toml
    [ -f "$BACKUP_DIR/Cargo.lock" ] && cp "$BACKUP_DIR/Cargo.lock" Cargo.lock
  fi
  rm -rf "$BACKUP_DIR"
}
trap 'rc=$?; if [ $rc -ne 0 ]; then restore_files; fi; rm -rf "$BACKUP_DIR"' EXIT

if [ "$VERSION" != "$CURRENT" ]; then
  NEEDS_RESTORE=1
  step "写入版本号 $CURRENT → $VERSION"
  # 只改 [workspace.package] 段里的 version：匹配到该段的第一个 version 行。
  perl -0pi -e "s/(\[workspace\.package\]\n(?:.*\n)*?version = \")[^\"]*(\")/\${1}$VERSION\${2}/" Cargo.toml
  [ "$(current_version)" = "$VERSION" ] || die "版本号写入失败，请检查 Cargo.toml 的 [workspace.package] 段"
  # 刷新 Cargo.lock 里 mo-* 的版本记录，保证 CI 用 --locked 也能过。
  cargo metadata --format-version 1 >/dev/null
  VERSION_COMMIT=1
else
  step "版本号保持 $VERSION"
  VERSION_COMMIT=0
fi

# ---------------------------------------------------------------- 确认并推送
printf '\n\033[1m即将发布\033[0m\n'
printf '  版本：%s（当前 branch %s）\n' "$TAG" "$BRANCH"
if [ "$SKIP_CHECKS" -eq 1 ]; then
  printf '  门禁：\033[33m已跳过\033[0m\n'
else
  printf '  门禁：fmt / clippy / test 已通过\n'
fi
printf '  remote：%s  →  %s\n' "$REMOTE" "$(git remote get-url "$REMOTE")"
if [ "$VERSION_COMMIT" -eq 1 ]; then
  printf '  提交：chore(release): bump version to %s\n' "$VERSION"
fi
printf '  tag：%s（推送后自动触发 GitHub Release 流水线）\n' "$TAG"
if [ "$NO_PUSH" -eq 1 ]; then
  printf '  \033[33m--no-push：只本地打 tag，不推送\033[0m\n'
fi
printf '\n'
menu "确认发布 ${TAG}？（推送后自动触发 GitHub Release 流水线）" "确认发布" "取消"
[ "$MENU_INDEX" -eq 0 ] || die "已取消"

if [ "$VERSION_COMMIT" -eq 1 ]; then
  step "提交版本改动"
  git add Cargo.toml
  # Cargo.lock 对二进制 crate 应当入库（发布可复现）；若被 .gitignore 挡住，
  # 只提示而不强加 -f，避免越过用户的忽略策略。
  if git check-ignore -q Cargo.lock; then
    printf '\033[33m! Cargo.lock 被 .gitignore 忽略，未随本次提交入库；\n  建议删掉该忽略规则并提交，保证发布版本可复现\033[0m\n'
  else
    git add Cargo.lock
  fi
  git commit -m "chore(release): bump version to $VERSION"
fi

step "创建 tag $TAG"
git tag -a "$TAG" -m "Mo $TAG"

if [ "$NO_PUSH" -eq 1 ]; then
  printf '\n\033[33m本地已就绪，手动推送：\033[0m\n  git push %s %s && git push %s %s\n' \
    "$REMOTE" "$BRANCH" "$REMOTE" "$TAG"
  exit 0
fi

step "推送到 $REMOTE"
git push "$REMOTE" "$BRANCH"
git push "$REMOTE" "$TAG"

REPO_URL="$(git remote get-url "$REMOTE" | sed -e 's#^git@github.com:#https://github.com/#' -e 's#\.git$##')"
cat <<EOF

✓ $TAG 已推送，GitHub Release 流水线开始跑（可能 10~20 分钟）：
  $REPO_URL/actions

产物就绪后会出现在：
  $REPO_URL/releases/tag/$TAG
EOF
