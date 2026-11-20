<#
.SYNOPSIS
    实时跟随每日流程日志（tanvec-ai-cn）

.DESCRIPTION
    daily-translate.ps1 现在是逐行实时写日志的，本脚本用来跟看。

    用法：
        .\watch-log.ps1                  # 打印今天日志的最后 30 行后退出
        .\watch-log.ps1 -Follow          # 实时跟随，直到流程结束标记出现
        .\watch-log.ps1 -Date 2026-10-24 -Tail 100
        .\watch-log.ps1 -Follow -Raw     # 跟随但不等结束标记，只能 Ctrl+C

    跟随时会自动在「每日流程成功 / 失败 / 自检结束」标记处收尾退出。

    Ctrl+C 可随时中断，不影响正在运行的流程。
#>

[CmdletBinding()]
param(
    # 直接指定日志文件；不给则按 -Date 或今天推导
    [string]$Path,

    # 指定日期，格式 yyyy-MM-dd
    [string]$Date,

    # 首次显示的末尾行数
    [int]$Tail = 30,

    # 实时跟随
    [switch]$Follow,

    # 跟随时不在结束标记处退出（需手动 Ctrl+C）
    [switch]$Raw,

    # 轮询间隔（毫秒）
    [int]$IntervalMs = 400
)

$RepoRoot = Split-Path -Parent $PSScriptRoot
$LogDir   = Join-Path $RepoRoot 'logs'
$StateFile = Join-Path $LogDir '.run-state'

# 输出被重定向到文件时也保持 UTF-8，避免中文变乱码
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }

if (-not $Path) {
    if ($Date) {
        $Path = Join-Path $LogDir ("daily-{0}.log" -f $Date)
    }
    else {
        $today = Join-Path $LogDir ("daily-{0}.log" -f (Get-Date -Format 'yyyy-MM-dd'))
        if (Test-Path -LiteralPath $today) {
            $Path = $today
        }
        else {
            $latest = Get-ChildItem -LiteralPath $LogDir -Filter 'daily-*.log' -File -ErrorAction SilentlyContinue |
                Sort-Object Name -Descending | Select-Object -First 1
            if ($latest) { $Path = $latest.FullName }
        }
    }
}

if (-not $Path -or -not (Test-Path -LiteralPath $Path)) {
    Write-Host "找不到日志文件：$Path" -ForegroundColor Red
    Write-Host "可用的日志："
    Get-ChildItem -LiteralPath $LogDir -Filter 'daily-*.log' -File -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 5 | ForEach-Object { "  " + $_.Name }
    exit 1
}

$Path = (Resolve-Path -LiteralPath $Path).Path

Write-Host ("日志: {0}" -f $Path) -ForegroundColor DarkGray
$fi = Get-Item -LiteralPath $Path
Write-Host ("大小: {0:N1} KB    最后写入: {1}" -f ($fi.Length / 1KB), $fi.LastWriteTime) -ForegroundColor DarkGray

if (Test-Path -LiteralPath $StateFile) {
    $st = Get-Content -LiteralPath $StateFile -Encoding UTF8
    $stateLog = ($st | Where-Object { $_ -like 'log=*' }) -replace '^log=', ''
    # 只有面包屑指向当前日志时才展示，避免看到别的运行的残留状态
    if ($stateLog -eq $Path) {
        $status = ($st | Where-Object { $_ -like 'status=*' }) -replace '^status=', ''
        $step   = ($st | Where-Object { $_ -like 'step=*' })   -replace '^step=', ''
        $upd    = ($st | Where-Object { $_ -like 'updated_local=*' }) -replace '^updated_local=', ''
        $color = if ($status -eq 'running') { 'Yellow' } else { 'DarkGray' }
        Write-Host ("状态: {0}    步骤: {1}    最后活动: {2}" -f $status, $step, $upd) -ForegroundColor $color
    }
}
Write-Host ('-' * 78) -ForegroundColor DarkGray

# 流程结束标记：跟随时见到就自动收尾
$endPattern = '^.*={5,} *(每日流程成功|每日流程失败|每日流程结束|自检结束)'

$lines = @(Get-Content -LiteralPath $Path -Encoding UTF8)
$offset = [Math]::Max(0, $lines.Count - $Tail)
for ($i = $offset; $i -lt $lines.Count; $i++) {
    Write-Host $lines[$i]
}

if (-not $Follow) {
    Write-Host ('-' * 78) -ForegroundColor DarkGray
    Write-Host "提示: 加 -Follow 可实时跟随（流程结束自动退出）" -ForegroundColor DarkGray
    exit 0
}

Write-Host ('-' * 78) -ForegroundColor DarkGray
Write-Host "实时跟随中…… Ctrl+C 退出" -ForegroundColor DarkGray

while ($true) {
    Start-Sleep -Milliseconds $IntervalMs

    if (-not (Test-Path -LiteralPath $Path)) {
        Write-Host "日志文件被删除，退出跟随" -ForegroundColor Yellow
        exit 1
    }

    $all = @(Get-Content -LiteralPath $Path -Encoding UTF8)

    # 文件被截断/轮转：从头再来
    if ($all.Count -lt $offset) {
        Write-Host "日志被截断，从头跟随" -ForegroundColor Yellow
        $offset = 0
    }

    if ($all.Count -gt $offset) {
        $hitEnd = $false
        for ($i = $offset; $i -lt $all.Count; $i++) {
            $line = $all[$i]
            Write-Host $line
            if (-not $Raw -and $line -match $endPattern) { $hitEnd = $true }
        }
        $offset = $all.Count
        if ($hitEnd) {
            Write-Host ('-' * 78) -ForegroundColor DarkGray
            Write-Host "流程已结束，停止跟随" -ForegroundColor Green
            exit 0
        }
    }
}
