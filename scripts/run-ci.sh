#!/usr/bin/env bash
#
# 一键复刻 GitHub Actions 里的质量门禁（.github/workflows/ci.yml）。
#
# CI 在 macOS 上跑 4 个检查项，本脚本按相同顺序、相同参数逐条执行：
#   1. 格式检查   cargo fmt --all -- --check
#   2. 编译检查   cargo check --all-targets --all-features
#   3. Clippy     cargo clippy --all-targets --all-features -- -D warnings
#   4. 测试       cargo test --all-features
#
# 行为与 CI 一致：任一步骤失败即中止（退出码非 0）。加 -k / --keep-going
# 可跑完全部步骤再汇总，方便一次性看清 fmt / clippy / test 各自的问题。
#
# 用法：
#   ./scripts/run-ci.sh                 # 复刻 CI，遇错即停
#   ./scripts/run-ci.sh -k              # 跑完所有检查再汇报
#   ./scripts/run-ci.sh --keep-going
#   ./scripts/run-ci.sh --retry-flaky   # 测试步骤失败时重试一次（仅本地，默认关）
#   ./scripts/run-ci.sh -h              # 看这段用法
#
# 注意：只格式化检查（-- --check）不改文件；想直接格式化用 `cargo fmt --all`。
#
# ⚠️ 本脚本要兼容 macOS 自带的 /bin/bash 3.2：**插值一律写 ${var}**。
#    3.2 会把紧跟裸变量名（$var）之后的多字节字符当成变量名的一部分，
#    于是 `$rc）` 会报 `rc）: unbound variable`。bash 5.x 无此问题，别只在
#    沙箱里验证。
#
# 关于 --retry-flaky：本项目已知 gpui 测试调度器会在**清理阶段**偶发 panic
#   （`test_scheduler` 报 local task dropped by a thread that didn't spawn it，
#   最终 SIGABRT）。它落在哪条测试名上是随机的，所以 `--skip <名字>` 挡不住，
#   只能整步重跑。CI 上同样是概率性红；默认不重试、与 CI 严格一致。

set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

# ---------------------------------------------------------------- 参数解析
KEEP_GOING=0
RETRY_FLAKY=0
for arg in "$@"; do
  case "$arg" in
    -h | --help)
      # 打印文件头那段注释（首个非注释行即止），不写死行号。
      awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
      exit 0
      ;;
    -k | --keep-going) KEEP_GOING=1 ;;
    --retry-flaky) RETRY_FLAKY=1 ;;
    *)
      printf '\033[31m✗ 未知参数：%s\033[0m\n' "$arg" >&2
      printf '用法：./scripts/run-ci.sh [-k|--keep-going] [--retry-flaky]\n' >&2
      exit 2
      ;;
  esac
done

# ---------------------------------------------------------------- 基础 helper
step() { printf '\n\033[36m→ %s\033[0m\n' "$1"; }
ok() { printf '\033[32m✓ %s\033[0m\n' "$1"; }
fail() { printf '\033[31m✗ %s\033[0m\n' "$1" >&2; }
die() { printf '\033[31m✗ %s\033[0m\n' "$1" >&2; exit 1; }

command -v cargo >/dev/null || die "需要 cargo（且 rustup 组件 rustfmt / clippy 已装）"

# ---------------------------------------------------------------- 检查项定义
# 每项：名称 | cargo 命令。顺序与 ci.yml 保持一致。
STEPS=(
  "格式检查|cargo fmt --all -- --check"
  "编译检查|cargo check --all-targets --all-features"
  "Clippy（告警即失败）|cargo clippy --all-targets --all-features -- -D warnings"
  "测试|cargo test --all-features"
)

# 「测试」那一步的显示名——只有它会被 --retry-flaky 重试。
TEST_STEP="测试"

# ---------------------------------------------------------------- 执行
failed=()
start_all=$(date +%s)

for entry in "${STEPS[@]}"; do
  name="${entry%%|*}"
  cmd="${entry#*|}"

  # 只有「测试」可能偶发（见文件头关于 gpui 调度器 flake 的说明）；
  # 格式 / 编译 / Clippy 是确定性的，重试没有意义。
  attempts=1
  if [ "${RETRY_FLAKY}" -eq 1 ] && [ "${name}" = "${TEST_STEP}" ]; then
    attempts=2
  fi

  step "${name}"
  printf '  $ %s\n' "${cmd}"
  s=$(date +%s)

  # 用 `&& / ||` 抓住退出码（比 `set +e; cmd; rc=$?; set -e` 少一次状态切换）。
  # 失败不立即退出：-k 要跑完，--retry-flaky 还要再试一次。
  rc=0
  n=0
  while [ "${n}" -lt "${attempts}" ]; do
    n=$((n + 1))
    eval "${cmd}" && rc=0 || rc=$?
    [ "${rc}" -eq 0 ] && break
    if [ "${n}" -lt "${attempts}" ]; then
      printf '\033[33m~ 首次失败（退出码 %s），按 --retry-flaky 重试一次…\033[0m\n' "${rc}"
    fi
  done

  e=$(date +%s)
  elapsed=$((e - s))

  if [ "${rc}" -ne 0 ]; then
    # ⚠️ 变量必须写成 ${rc} 而不是 $rc：macOS 自带的 /bin/bash 3.2 会把紧跟
    # 裸变量名之后的多字节字符（这里是「）」）当成变量名的一部分，于是报
    # `rc）: unbound variable`（sandbox 里 bash 5.x 不复现，别在那儿试）。
    fail "${name} 失败（用时 ${elapsed}s，退出码 ${rc}）"
    failed+=("${name}")
    if [ "${KEEP_GOING}" -eq 0 ]; then
      printf '\n\033[31m门禁未通过，已在「%s」中止（与 CI 行为一致）。\033[0m\n' "${name}"
      printf '单独重跑该步骤：\033[36m%s\033[0m\n' "${cmd}"
      exit 1
    fi
  elif [ "${n}" -gt 1 ]; then
    ok "${name} 通过（用时 ${elapsed}s，首次为已知 flake、重试后通过）"
  else
    ok "${name} 通过（用时 ${elapsed}s）"
  fi
done

end_all=$(date +%s)
total=$((end_all - start_all))

# ---------------------------------------------------------------- 汇总
if [ "${#failed[@]}" -gt 0 ]; then
  printf '\n\033[31m✗ 共 %d 项失败：%s\033[0m\n' "${#failed[@]}" "$(IFS=、; echo "${failed[*]}")"
  exit 1
fi

printf '\n\033[32m✓ 所有检查通过（总用时 %ss），与 ci.yml 一致。\033[0m\n' "$total"
