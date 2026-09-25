#Requires -Version 5.1
#
# 发布 tag（PowerShell 版，与 release-tag.sh 对等）：
# 更新版本号 → 跑质量门禁 → 提交 → 打 tag → 推送。
#
# 推送 tag 会自动触发 .github/workflows/release.yml：各平台打包 +
# 创建 GitHub Release（产物下载与 release notes 都在那里）。
#
# 用法：
#   .\scripts\release-tag.ps1                 # 用 Cargo.toml 里现有版本号打 tag
#   .\scripts\release-tag.ps1 patch           # 0.1.0 → 0.1.1
#   .\scripts\release-tag.ps1 minor           # 0.1.0 → 0.2.0
#   .\scripts\release-tag.ps1 major           # 0.1.0 → 1.0.0
#   .\scripts\release-tag.ps1 beta            # 0.1.0 → 0.1.1-beta.1；0.1.1-beta.1 → 0.1.1-beta.2
#   .\scripts\release-tag.ps1 1.2.3           # 显式指定
#   .\scripts\release-tag.ps1 0.1.1-beta.3    # 显式指定 beta 版
#   .\scripts\release-tag.ps1 patch -NoPush   # 只本地打 tag，稍后自己推
#   .\scripts\release-tag.ps1 patch -SkipChecks  # 跳过 fmt/clippy/test，快速发布
#
# beta 版即 semver 预发布形态 `x.y.z-beta.N`：tag 形如 v0.1.1-beta.1，
# GitHub Release 会被流水线标成 Pre-release。要把某个 beta 转正式，显式传
# 对应版本号（如 `0.1.1`）；`patch/minor/major` 永远在数字段上 +1（会先剥掉
# 现有的 `-beta.N` 再进位，不会原地转正）。
#
# 质量门禁与最终确认都是方向键菜单（↑/↓ 移动光标，回车确认，默认选中
# 第一项）；选「跳过」直接发布，push 后 CI 仍会兜底跑一遍。
#
# 唯一 versions 真源：[workspace.package].version（各 crate 都是
# version.workspace = true），所以只改这一处。
#
# ⚠️ 文件必须存成 **UTF-8 带 BOM**：Windows PowerShell 5.1 会把无 BOM 的
# UTF-8 当 ANSI 读，中文提示会花屏。

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$Bump = "",
    [switch]$NoPush,
    [switch]$SkipChecks
)

# 这里刻意不用 $ErrorActionPreference='Stop'：PS 5.1 下它会要了 git/cargo 的
# 命——这些原生命令把进度写 stderr，管道到 2>&1 时会被当成终止性错误抛出。
# 外置命令一律显式查 $LASTEXITCODE，行为与 bash 的 `|| die` 对等。
$ErrorActionPreference = 'Continue'

Set-Location (Split-Path -Parent (Split-Path -Parent $PSCommandPath))

function Die([string]$msg) {
    Write-Host "✗ $msg" -ForegroundColor Red
    exit 1
}
function Step([string]$msg) { Write-Host "→ $msg" -ForegroundColor Cyan }

# 方向键菜单：↑/↓ 移动光标，回车确认，Ctrl+C 退出；返回选中下标（0 起）。
function Menu([string]$prompt, [string[]]$items) {
    if ([Console]::IsInputRedirected) {
        # 非交互环境（管道 / CI）里硬弹菜单等于挂死，明确拒绝。
        Die "Menu 需要交互式终端（非交互场景请配合 -SkipChecks 使用）"
    }
    Write-Host $prompt
    $sel = 0
    $top = [Console]::CursorTop
    $first = $true
    while ($true) {
        if (-not $first) {
            [Console]::CursorTop = $top
            [Console]::CursorLeft = 0
        }
        $first = $false
        for ($i = 0; $i -lt $items.Count; $i++) {
            $line = if ($i -eq $sel) { "❯ $($items[$i])" } else { "  $($items[$i])" }
            $pad = ' ' * [Math]::Max(0, [Console]::WindowWidth - $line.Length - 2)
            if ($i -eq $sel) { Write-Host ($line + $pad) -ForegroundColor Cyan }
            else { Write-Host ($line + $pad) }
        }
        $ki = [Console]::ReadKey($true)
        if ($ki.Key -eq [ConsoleKey]::C -and
            ($ki.Modifiers -band [ConsoleModifiers]::Control)) { exit 130 }
        if ($ki.Key -eq [ConsoleKey]::UpArrow) {
            if ($sel -gt 0) { $sel-- }
        } elseif ($ki.Key -eq [ConsoleKey]::DownArrow) {
            if ($sel -lt $items.Count - 1) { $sel++ }
        } elseif ($ki.Key -eq [ConsoleKey]::Enter -or
                  $ki.Key -eq [ConsoleKey]::NumberPadEnter) {
            return $sel
        }
    }
}

function Get-Version {
    $text = [IO.File]::ReadAllText("Cargo.toml")
    $m = [regex]::Match($text, '\[workspace\.package\][\s\S]*?version = "([^"]+)"')
    if (-not $m.Success) { Die "无法从 Cargo.toml 读取 [workspace.package].version" }
    return $m.Groups[1].Value
}

# ---------------------------------------------------------------- 前置检查
foreach ($cmd in 'git', 'cargo') {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) { Die "需要 $cmd" }
}

& git rev-parse --git-dir 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Die "当前不在 git 仓库里" }

$status = & git status --porcelain 2>&1 | Where-Object { "$_".Trim() }
if ($status) { Die "工作区有未提交改动，请先提交或 stash（git status 查看详情）" }

$remotes = @(& git remote 2>&1 | ForEach-Object { "$_".Trim() } | Where-Object { $_ })
$Remote = if ($remotes -contains 'origin') { 'origin' } elseif ($remotes.Count) { $remotes[0] } else { $null }
if (-not $Remote) {
    Die "没有配置 git remote，先加一个：git remote add origin <你的 GitHub 仓库 URL>"
}

$Branch = & git rev-parse --abbrev-ref HEAD 2>&1
if ($LASTEXITCODE -ne 0) { Die "读当前分支失败：$Branch" }

$Current = Get-Version

# ---------------------------------------------------------------- 版本计算
# 把现有版本号拆成「数字段 + 预发布段」：`0.1.1-beta.2` → base=`0.1.1`、
# pre=`beta.2`；纯正式版本 pre 为空。
$CurBase = $Current.Split('-')[0]
$CurPre = ''
if ($Current.Contains('-')) { $CurPre = $Current.Substring($CurBase.Length + 1) }

if ($Bump) {
    if ($Bump -match '^(patch|minor|major|beta)$') {
        $p = $CurBase.Split('.') | ForEach-Object { [int]$_ }
        if ($p.Count -lt 3) { Die "当前版本号不是 x.y.z 形态：$Current" }
        switch ($Bump) {
            # patch/minor/major 在数字段上进位（先剥掉 `-beta.N`，所以 beta 不会
            # 「原地转正」——转正请显式传版本号）。
            'patch' { $Version = "$($p[0]).$($p[1]).$($p[2] + 1)" }
            'minor' { $Version = "$($p[0]).$($p[1] + 1).0" }
            'major' { $Version = "$($p[0] + 1).0.0" }
            'beta' {
                if ($CurPre -match '^beta\.(\d+)$') {
                    # 已经是 beta：只进预发布号（0.1.1-beta.1 → 0.1.1-beta.2）。
                    $Version = "$CurBase-beta.$([int]$Matches[1] + 1)"
                } else {
                    # 从正式版本起 beta：patch 进位后挂 `-beta.1`。
                    $Version = "$($p[0]).$($p[1]).$($p[2] + 1)-beta.1"
                }
            }
        }
    } elseif ($Bump -match '^\d+\.\d+\.\d+(-beta\.\d+)?$') {
        $Version = $Bump
    } else {
        Die "无法识别的版本参数：$Bump（可用 patch / minor / major / beta / x.y.z[-beta.N]）"
    }
} else {
    $Version = $Current
}

$Tag = "v$Version"

& git rev-parse -q --verify "refs/tags/$Tag" 2>&1 | Out-Null
if ($LASTEXITCODE -eq 0) { Die "tag $Tag 已存在" }

# ---------------------------------------------------------------- 质量门禁
# 先在**改文件之前**跑门禁：不合格时工作区仍是干净的，不必手动回滚。
if (-not $SkipChecks) {
    $idx = Menu "质量门禁 fmt / clippy / test 可能需要几分钟" `
        @('运行质量门禁（推荐）', '跳过，快速发布（push 后 CI 兜底）')
    if ($idx -eq 1) {
        $SkipChecks = $true
        Write-Host '! 已跳过质量门禁；push 后 CI 仍会跑一遍，留意 Actions 结果' -ForegroundColor Yellow
    }
}

if (-not $SkipChecks) {
    Step "质量门禁：fmt / clippy / test"
    & cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { Die "cargo fmt --check 未过（先跑 cargo fmt --all）" }
    & cargo clippy --all-targets --all-features -- -D warnings
    if ($LASTEXITCODE -ne 0) { Die "cargo clippy 未过" }
    & cargo test --all-features
    if ($LASTEXITCODE -ne 0) { Die "cargo test 未过" }
}

# ---------------------------------------------------------------- 版本写入
# 写版本前备份，中途失败自动还原（避免留下一份半改的 Cargo.toml）。
$backupDir = Join-Path ([IO.Path]::GetTempPath()) ("mo-release-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $backupDir | Out-Null
Copy-Item Cargo.toml $backupDir
$hadLock = Test-Path Cargo.lock
if ($hadLock) { Copy-Item Cargo.lock $backupDir }
$versionCommit = $false
$committed = $false
$ok = $false

try {
    if ($Version -ne $Current) {
        Step "写入版本号 $Current → $Version"
        $text = [IO.File]::ReadAllText("Cargo.toml")
        $re = [regex]'(\[workspace\.package\][\s\S]*?version = ")[^"]*(")'
        # 只换第一处命中（[workspace.package] 后的第一个 version 行），与 bash 版
        # perl 替换语义一致；替换串里的 ${1}/${2} 是捕获组引用。
        $text = $re.Replace($text, ('${1}' + $Version + '${2}'), 1)
        # 不带 BOM 的 UTF-8 写回，行尾随原文（-replace 不碰换行）。
        [IO.File]::WriteAllText("Cargo.toml", $text, (New-Object Text.UTF8Encoding($false)))
        if ((Get-Version) -ne $Version) { Die "版本号写入失败，请检查 Cargo.toml 的 [workspace.package] 段" }
        # 刷新 Cargo.lock 里 mo-* 的版本记录，保证 CI 用 --locked 也能过。
        & cargo metadata --format-version 1 1>$null 2>&1
        if ($LASTEXITCODE -ne 0) { Die "cargo metadata 失败，Cargo.lock 没刷新" }
        $versionCommit = $true
    } else {
        Step "版本号保持 $Version"
    }

    # ------------------------------------------------------------ 确认并推送
    Write-Host ''
    Write-Host '即将发布' -ForegroundColor White
    Write-Host "  版本：$Tag（当前 branch $Branch）"
    if ($Version -match '-beta\.') {
        Write-Host '  beta 版：GitHub Release 会标为 Pre-release' -ForegroundColor Yellow
    }
    if ($SkipChecks) {
        Write-Host '  门禁：已跳过' -ForegroundColor Yellow
    } else {
        Write-Host '  门禁：fmt / clippy / test 已通过'
    }
    Write-Host "  remote：$Remote  →  $(& git remote get-url $Remote 2>&1)"
    if ($versionCommit) {
        Write-Host "  提交：chore(release): bump version to $Version"
    }
    Write-Host "  tag：$Tag（推送后自动触发 GitHub Release 流水线）"
    if ($NoPush) {
        Write-Host '  -NoPush：只本地打 tag，不推送' -ForegroundColor Yellow
    }
    Write-Host ''

    $idx = Menu "确认发布 $Tag？（推送后自动触发 GitHub Release 流水线）" @('确认发布', '取消')
    if ($idx -ne 0) { Die '已取消' }

    if ($versionCommit) {
        Step "提交版本改动"
        & git add Cargo.toml
        if ($LASTEXITCODE -ne 0) { Die 'git add Cargo.toml 失败' }
        # Cargo.lock 对二进制 crate 应当入库（发布可复现）；若被 .gitignore 挡住，
        # 只提示而不强加 -f，避免越过用户的忽略策略。
        & git check-ignore -q Cargo.lock 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) {
            Write-Host '! Cargo.lock 被 .gitignore 忽略，未随本次提交入库；' -ForegroundColor Yellow
            Write-Host '  建议删掉该忽略规则并提交，保证发布版本可复现' -ForegroundColor Yellow
        } elseif ($hadLock) {
            & git add Cargo.lock
            if ($LASTEXITCODE -ne 0) { Die 'git add Cargo.lock 失败' }
        }
        & git commit -m "chore(release): bump version to $Version"
        if ($LASTEXITCODE -ne 0) { Die 'git commit 失败' }
        $committed = $true   # 版本改动已进提交，回滚不再是「还原文件」的事
    }

    Step "创建 tag $Tag"
    & git tag -a $Tag -m "Mo $Tag"
    if ($LASTEXITCODE -ne 0) { Die "git tag 失败" }

    $ok = $true   # 从这里起文件状态是「已提交」，不需要还原

    if ($NoPush) {
        Write-Host ''
        Write-Host '本地已就绪，手动推送：' -ForegroundColor Yellow
        Write-Host "  git push $Remote $Branch; git push $Remote $Tag"
        exit 0
    }

    Step "推送到 $Remote"
    & git push $Remote $Branch 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { Die "git push 分支失败（tag $Tag 已在本地，修好后手动：git push $Remote $Tag）" }
    & git push $Remote $Tag 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { Die "git push tag 失败，手动重试：git push $Remote $Tag" }

    $repoUrl = (& git remote get-url $Remote 2>&1) -replace '^git@github\.com:', 'https://github.com/' -replace '\.git$', ''
    Write-Host ''
    Write-Host "✓ $Tag 已推送，GitHub Release 流水线开始跑（可能 10~20 分钟）：" -ForegroundColor Green
    Write-Host "  $repoUrl/actions"
    Write-Host ''
    Write-Host '产物就绪后会出现在：'
    Write-Host "  $repoUrl/releases/tag/$Tag"
} finally {
    # 中途失败且**版本改动还没进提交**时，把 Cargo.toml / Cargo.lock 还原干净
    # （对齐 bash 版的 trap）。已提交后不回滚工作区——那半截该交给 git 处理。
    if (-not $ok -and $versionCommit -and -not $committed) {
        Write-Host '↩ 中途失败，已还原 Cargo.toml / Cargo.lock' -ForegroundColor Yellow
        Copy-Item (Join-Path $backupDir 'Cargo.toml') Cargo.toml -Force
        if ($hadLock -and (Test-Path (Join-Path $backupDir 'Cargo.lock'))) {
            Copy-Item (Join-Path $backupDir 'Cargo.lock') Cargo.lock -Force
        }
    }
    Remove-Item -Recurse -Force $backupDir -ErrorAction SilentlyContinue
}
