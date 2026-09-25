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
#   ./scripts/release-tag.sh major           # 0.1.0 → 1.0.0
#   ./scripts/release-tag.sh beta            # 0.1.0 → 0.1.1-beta.1；0.1.1-beta.1 → 0.1.1-beta.2
#   ./scripts/release-tag.sh 1.2.3         # 显式指定
#   ./scripts/release-tag.sh 0.1.1-beta.3  # 显式指定 beta 版
#   ./scripts/release-tag.sh patch --no-push   # 只本地打 tag，稍后自己推
#   ./scripts/release-tag.sh patch --skip-checks  # 跳过 fmt/clippy/test，快速发布
#   ./scripts/release-tag.sh --republish       # 重发当前版本号对应的 tag
#
# --republish：某次 release 流水线挂了（构建失败 / 产物坏了）时用——把已有的
# tag 移到当前 HEAD（带上修复后的代码），删掉远端旧 tag 再重推，重新触发一遍
# 流水线。不改版本号、不产生新提交；远端 tag 删除会把旧 Release 打成草稿，
# 重跑完由流水线重新发布。要重发**旧版本**（Cargo.toml 已经往前走了）别用
# 这条，去 Actions 面板对 release.yml 手动 Run workflow、填旧 tag。
#
# beta 版即 semver 预发布形态 `x.y.z-beta.N`：tag 形如 v0.1.1-beta.1，
# GitHub Release 会被流水线标成 Pre-release。要把某个 beta 转正式，显式传
# 对应版本号（如 `0.1.1`）；`patch/minor/major` 永远在数字段上 +1（会先剥掉
# 现有的 `-beta.N` 再进位，不会原地转正）。
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
REPUBLISH=0

usage() {
  # 打印文件头的注释块（第 3 行起，到 `set -euo` 前为止）——锚点匹配，
  # 免得注释块行长变化后写死的行号把用法说明切掉半截。
  sed -n '3,/^set -euo/p' "$0" | sed -e '/^set -euo/d' -e 's/^# \{0,1\}//'
  exit "${1:-0}"
}

for arg in "$@"; do
  case "$arg" in
    -h | --help) usage 0 ;;
    --no-push) NO_PUSH=1 ;;
    --skip-checks) SKIP_CHECKS=1 ;;
    --republish) REPUBLISH=1 ;;
    patch | minor | major | beta) BUMP="$arg" ;;
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

# 把现有版本号拆成「数字段 + 预发布段」：`0.1.1-beta.2` → BASE=`0.1.1`、
# PRE=`beta.2`；纯正式版本 PRE 为空。
CURRENT_BASE="${CURRENT%%-*}"
CURRENT_PRE=""
[ "$CURRENT" = "$CURRENT_BASE" ] || CURRENT_PRE="${CURRENT#*-}"

if [ -n "$BUMP" ]; then
  case "$BUMP" in
    beta)
      if [[ "$CURRENT_PRE" == beta.* ]]; then
        # 已经是 beta：只进预发布号（0.1.1-beta.1 → 0.1.1-beta.2）。
        N="${CURRENT_PRE#beta.}"
        VERSION="${CURRENT_BASE}-beta.$((N + 1))"
      else
        # 从正式版本起 beta：patch 进位后挂 `-beta.1`。
        IFS='.' read -r MA MI PA <<<"$CURRENT_BASE"
        VERSION="$MA.$MI.$((PA + 1))-beta.1"
      fi
      ;;
    patch | minor | major)
      # 在数字段上进位（先剥掉 `-beta.N`，所以 beta 不会「原地转正」——
      # 转正请显式传版本号）。
      IFS='.' read -r MA MI PA <<<"$CURRENT_BASE"
      case "$BUMP" in
        patch) PA=$((PA + 1)) ;;
        minor) MI=$((MI + 1)); PA=0 ;;
        major) MA=$((MA + 1)); MI=0; PA=0 ;;
      esac
      VERSION="$MA.$MI.$PA"
      ;;
    *)
      if [[ "$BUMP" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$ ]]; then
        VERSION="$BUMP"
      else
        die "无法识别的版本参数：${BUMP}（可用 patch / minor / major / beta / x.y.z[-beta.N]）"
      fi
      ;;
  esac
else
  VERSION="$CURRENT"
fi

TAG="v$VERSION"

# tag 现状：本地查 refs，远端只在重发时才 ls-remote（正常发布不必多一次网络往返）。
LOCAL_HAS_TAG=0
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && LOCAL_HAS_TAG=1
REMOTE_HAS_TAG=0

if [ "$REPUBLISH" -eq 1 ]; then
  [ -z "$BUMP" ] || die "--republish 不接受版本参数（重发不改版本号）"
  if git ls-remote --tags "$REMOTE" "refs/tags/$TAG" 2>/dev/null | grep -q .; then
    REMOTE_HAS_TAG=1
  fi
  if [ "$LOCAL_HAS_TAG" -eq 0 ] && [ "$REMOTE_HAS_TAG" -eq 0 ]; then
    die "--republish：tag $TAG 本地和远端都不存在，没有可重发的（要打新 tag 去掉 --republish 即可）"
  fi
  printf '\033[33m! 重发 %s：tag 会移到当前 HEAD（带上修复后的代码），不改版本号\033[0m\n' "$TAG"
else
  [ "$LOCAL_HAS_TAG" -eq 0 ] || die "tag $TAG 已存在（要重发它：加 --republish）"
fi

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
case "$VERSION" in
  *-beta.*) printf '  \033[33mbeta 版：GitHub Release 会标为 Pre-release\033[0m\n' ;;
esac
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
if [ "$REPUBLISH" -eq 1 ]; then
  printf '  \033[33m重发：删掉已有 tag 重新打；远端旧 tag 删除会把旧 Release 打成草稿\033[0m\n'
fi
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

if [ "$LOCAL_HAS_TAG" -eq 1 ]; then
  step "删除本地旧 tag $TAG（重新指到当前 HEAD）"
  git tag -d "$TAG"
fi

step "创建 tag $TAG"
git tag -a "$TAG" -m "Mo $TAG"

if [ "$NO_PUSH" -eq 1 ]; then
  printf '\n\033[33m本地已就绪，手动推送：\033[0m\n'
  if [ "$REMOTE_HAS_TAG" -eq 1 ]; then
    printf '  git push %s :refs/tags/%s && \\\n' "$REMOTE" "$TAG"
  fi
  printf '  git push %s %s && git push %s %s\n' "$REMOTE" "$BRANCH" "$REMOTE" "$TAG"
  exit 0
fi

step "推送到 $REMOTE"
if [ "$REMOTE_HAS_TAG" -eq 1 ]; then
  step "删除远端旧 tag $TAG（旧 Release 变草稿，流水线重跑后重新发布）"
  git push "$REMOTE" ":refs/tags/$TAG"
fi
git push "$REMOTE" "$BRANCH"
git push "$REMOTE" "$TAG"

REPO_URL="$(git remote get-url "$REMOTE" | sed -e 's#^git@github.com:#https://github.com/#' -e 's#\.git$##')"
cat <<EOF

✓ $TAG 已推送，GitHub Release 流水线开始跑（可能 10~20 分钟）：
  $REPO_URL/actions

产物就绪后会出现在：
  $REPO_URL/releases/tag/$TAG
EOF
