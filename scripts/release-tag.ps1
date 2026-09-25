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
#                                             #（tag 已存在时弹交互菜单：发新版本 / 重发 / 取消）
#   .\scripts\release-tag.ps1 patch           # 0.1.0 → 0.1.1
#   .\scripts\release-tag.ps1 minor           # 0.1.0 → 0.2.0
#   .\scripts\release-tag.ps1 major           # 0.1.0 → 1.0.0
#   .\scripts\release-tag.ps1 beta            # 0.1.0 → 0.1.1-beta.1；0.1.1-beta.1 → 0.1.1-beta.2
#   .\scripts\release-tag.ps1 1.2.3           # 显式指定
#   .\scripts\release-tag.ps1 0.1.1-beta.3    # 显式指定 beta 版
#   .\scripts\release-tag.ps1 patch -NoPush   # 只本地打 tag，稍后自己推
#   .\scripts\release-tag.ps1 patch -SkipChecks  # 跳过 fmt/clippy/test，快速发布
#   .\scripts\release-tag.ps1 -Republish      # 重发当前版本号对应的 tag
#
# 交互式：不带版本参数运行、且当前版本号对应的 tag 已存在（本地或远端）时，
# 会弹菜单让你选——「发布新版本（再选 patch/minor/major/beta）」「重新发布该
# tag」「取消」。显式传了版本参数则该场景直接报错，不兜圈子。
#
# -Republish：某次 release 流水线挂了（构建失败 / 产物坏了）时用——把已有的
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
    [switch]$SkipChecks,
    [switch]$Republish
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

# 进位规则（与 bash 版 bump_version 对等）：patch/minor/major 在数字段上进位
# （先剥掉 `-beta.N`，所以 beta 不会「原地转正」——转正请显式传版本号）；
# beta 已经是 beta 时只进预发布号，否则 patch 进位后挂 `-beta.1`。
function Get-BumpedVersion([string]$Kind) {
    $p = $CurBase.Split('.') | ForEach-Object { [int]$_ }
    if ($p.Count -lt 3) { Die "当前版本号不是 x.y.z 形态：$Current" }
    switch ($Kind) {
        'patch' { return "$($p[0]).$($p[1]).$($p[2] + 1)" }
        'minor' { return "$($p[0]).$($p[1] + 1).0" }
        'major' { return "$($p[0] + 1).0.0" }
        'beta' {
            if ($CurPre -match '^beta\.(\d+)$') {
                return "$CurBase-beta.$([int]$Matches[1] + 1)"
            }
            return "$($p[0]).$($p[1]).$($p[2] + 1)-beta.1"
        }
    }
}

function Test-TagLocal([string]$T) {
    & git rev-parse -q --verify "refs/tags/$T" 2>&1 | Out-Null
    return ($LASTEXITCODE -eq 0)
}

function Test-TagRemote([string]$T) {
    # tag 不存在时 ls-remote 也是退出码 0、输出为空；非零退出是连不上远端等真错误，
    # 这时当作「远端没有」处理，让后续报错给出准确指引。
    $out = @(& git ls-remote --tags $Remote "refs/tags/$T" 2>&1 |
        ForEach-Object { "$_".Trim() } | Where-Object { $_ })
    return ($LASTEXITCODE -eq 0 -and $out.Count -gt 0)
}

if ($Bump) {
    if ($Bump -match '^(patch|minor|major|beta)$') {
        $Version = Get-BumpedVersion $Bump
    } elseif ($Bump -match '^\d+\.\d+\.\d+(-beta\.\d+)?$') {
        $Version = $Bump
    } else {
        Die "无法识别的版本参数：$Bump（可用 patch / minor / major / beta / x.y.z[-beta.N]）"
    }
} else {
    $Version = $Current
}

$Tag = "v$Version"

# tag 现状：本地查 refs，远端只在本地查不到 / 重发时才 ls-remote（少弹网络往返）。
$localHasTag = Test-TagLocal $Tag
$remoteHasTag = $false

if ($Republish) {
    if ($Bump) { Die '-Republish 不接受版本参数（重发不改版本号）' }
    $remoteHasTag = Test-TagRemote $Tag
    if (-not $localHasTag -and -not $remoteHasTag) {
        Die "-Republish：tag $Tag 本地和远端都不存在，没有可重发的（要打新 tag 去掉 -Republish 即可）"
    }
    Write-Host "! 重发 $Tag：tag 会移到当前 HEAD（带上修复后的代码），不改版本号" -ForegroundColor Yellow
} else {
    # tag 已存在时：不带版本参数 → 交互菜单（发新版本 / 重发 / 取消）；带了版本
    # 参数说明是显式进位，不必再兜圈子，直接报错让用户明确意图。
    $tagExists = $localHasTag
    if (-not $tagExists) {
        $remoteHasTag = Test-TagRemote $Tag
        if ($remoteHasTag) { $tagExists = $true }
    }
    if ($tagExists) {
        if ($Bump) {
            Die "tag $Tag 已存在（进位后的版本也发布过）；去掉版本参数重跑可走交互菜单，或 -Republish 重发当前版本"
        }
        $idx = Menu "tag $Tag 已存在，怎么做？" `
            @('发布新版本（选进位方式）', "重新发布 $Tag（tag 移到当前 HEAD，重新触发流水线）", '取消')
        if ($idx -eq 2) { Die '已取消' }
        if ($idx -eq 1) {
            $Republish = $true
            Write-Host "! 重发 $Tag：tag 会移到当前 HEAD（带上修复后的代码），不改版本号" -ForegroundColor Yellow
        } else {
            $kinds = @('patch', 'minor', 'major', 'beta')
            $items = @($kinds | ForEach-Object { "$_ → $(Get-BumpedVersion $_)" }) + '取消'
            $j = Menu "选新版本号（当前 $Current）" $items
            if ($j -ge $kinds.Count) { Die '已取消' }
            $Version = Get-BumpedVersion $kinds[$j]
            $Tag = "v$Version"
            $localHasTag = Test-TagLocal $Tag
            $remoteHasTag = $false
            if (-not $localHasTag) { $remoteHasTag = Test-TagRemote $Tag }
            if ($localHasTag -or $remoteHasTag) {
                Die "tag $Tag 也已存在，换一种进位，或直接 -Republish 重发它"
            }
        }
    }
}

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
    if ($Republish) {
        Write-Host '  重发：删掉已有 tag 重新打；远端旧 tag 删除会把旧 Release 打成草稿' -ForegroundColor Yellow
    }
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

    if ($localHasTag) {
        Step "删除本地旧 tag $Tag（重新指到当前 HEAD）"
        & git tag -d $Tag
        if ($LASTEXITCODE -ne 0) { Die "git tag -d 失败" }
    }

    Step "创建 tag $Tag"
    & git tag -a $Tag -m "Mo $Tag"
    if ($LASTEXITCODE -ne 0) { Die "git tag 失败" }

    $ok = $true   # 从这里起文件状态是「已提交」，不需要还原

    if ($NoPush) {
        Write-Host ''
        Write-Host '本地已就绪，手动推送：' -ForegroundColor Yellow
        if ($remoteHasTag) {
            Write-Host "  git push $Remote :refs/tags/$Tag"
        }
        Write-Host "  git push $Remote $Branch; git push $Remote $Tag"
        exit 0
    }

    Step "推送到 $Remote"
    if ($remoteHasTag) {
        Step "删除远端旧 tag $Tag（旧 Release 变草稿，流水线重跑后重新发布）"
        & git push $Remote ":refs/tags/$Tag" 2>&1 | ForEach-Object { "$_" }
        if ($LASTEXITCODE -ne 0) { Die "删除远端旧 tag 失败，手动重试：git push $Remote :refs/tags/$Tag" }
    }
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
