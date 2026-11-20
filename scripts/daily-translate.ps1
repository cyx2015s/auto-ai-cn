<#
.SYNOPSIS
    每日 7:00 自动翻译 → 打包 → 上传（tanvec-ai-cn）

.DESCRIPTION
    依次执行：
        1. cargo r -r                     翻译管道（无子命令 = Translate）
        2. cargo r -r pack --protect      打包，保护模式
        3. cargo r -r upload --file <zip> 上传到 Mod Portal

    规则：
        - 任一步失败即停止，后续步骤不再执行（避免上传残缺包）。
        - --file 自动取 output\ 下最近修改的 .zip，不靠日期拼名字
          （pack 用 UTC 日期命名，本地 UTC+8 早晨跑会差一天）。
        - 子进程输出**逐行实时**写入日志，写入即 flush。
          可用 scripts\watch-log.ps1 -Follow 实时跟随。
        - 每步边界写 logs\.run-state 面包屑；若进程被外部杀掉，
          下次运行会报告「上次运行未正常结束」。

    由计划任务「tanvec-ai-cn daily translate-pack-upload」调用。
#>

[CmdletBinding()]
param(
    # 只做前期检查与文件选择，不真正执行三个步骤（供验证用）
    [switch]$DryRun,

    # 自检：用合成命令验证「流式输出」与「失败传播」，不碰 cargo
    [switch]$SelfTest,

    # 覆盖日志路径（默认 logs\daily-<本地日期>.log）
    [string]$LogPath
)

$ErrorActionPreference = 'Stop'
# 让父子进程的中文都按 UTF-8 解码
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }
# 原生命令的 stderr 只当普通输出，不当异常（保持旧行为）
$PSNativeCommandUseErrorActionPreference = $false

$RepoRoot  = Split-Path -Parent $PSScriptRoot
$OutputDir = Join-Path $RepoRoot 'output'
$LogDir    = Join-Path $RepoRoot 'logs'
$CargoExe  = 'C:\Users\mirac\.cargo\bin\cargo.exe'

$null = New-Item -ItemType Directory -Force -Path $LogDir
if (-not $LogPath) {
    $LogPath = Join-Path $LogDir ("daily-{0}.log" -f (Get-Date -Format 'yyyy-MM-dd'))
}
$StateFile = Join-Path $LogDir '.run-state'

# 日志流：AutoFlush 让每一行立刻落盘，watch-log / Get-Content -Wait 才能实时看到
$script:LogStream = [System.IO.StreamWriter]::new(
    $LogPath,
    $true,                                    # append
    [System.Text.UTF8Encoding]::new($false)   # UTF-8 无 BOM
)
$script:LogStream.AutoFlush = $true

function Write-Log {
    param([string]$Message, [string]$Level = 'INFO')
    $line = '{0} [{1}] {2}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $Level, $Message
    $script:LogStream.WriteLine($line)
    Write-Host $line
}

# 子进程输出：原样逐行透传（内部日志自带 UTC 时间戳和级别，不再二次加前缀）
function Write-ChildLine {
    param([string]$Line)
    $Line = $Line.TrimEnd("`r")
    if ($Line.Length -eq 0) { return }
    $script:LogStream.WriteLine($Line)
    Write-Host $Line
}

# 面包屑：进程被硬杀时 finally 不会执行，正因如此它才会留在 running 状态，
# 供下次运行检测「上次没跑完」
function Write-RunState {
    param([string]$Status, [string]$Step)
    $lines = @(
        "status=$Status"
        "pid=$PID"
        "step=$Step"
        "started_local=$($script:RunStartLocal.ToString('yyyy-MM-dd HH:mm:ss'))"
        "updated_local=$((Get-Date).ToString('yyyy-MM-dd HH:mm:ss'))"
        "updated_utc=$((Get-Date).ToUniversalTime().ToString('yyyy-MM-dd HH:mm:ss'))"
        "log=$LogPath"
    )
    try { Set-Content -LiteralPath $StateFile -Value $lines -Encoding UTF8 } catch { }
}

# 运行一个原生命令：输出逐行实时透传，失败抛异常
function Invoke-Step {
    param([string]$Name, [string]$Exe, [string[]]$Arguments)

    $shown = ($Arguments | ForEach-Object { if ($_ -match '[\s"]') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ } }) -join ' '
    Write-Log ("开始: {0}  =>  {1} {2}" -f $Name, $Exe, $shown)
    Write-RunState -Status 'running' -Step $Name
    $started = Get-Date

    & $Exe @Arguments 2>&1 | ForEach-Object { Write-ChildLine -Line $_.ToString() }

    $code = $LASTEXITCODE
    $elapsed = (Get-Date) - $started

    if ($code -ne 0) {
        Write-Log ("失败: {0}，退出码 {1}（耗时 {2:hh\:mm\:ss}）" -f $Name, $code, $elapsed) 'ERROR'
        throw ("{0} 失败，退出码 {1}" -f $Name, $code)
    }
    Write-Log ("完成: {0}，退出码 0（耗时 {1:hh\:mm\:ss}）" -f $Name, $elapsed)
}

function Import-DotEnv {
    param([string]$Path)
    if (-not (Test-Path $Path)) { throw "缺少 .env 文件: $Path" }
    $count = 0
    foreach ($raw in Get-Content $Path) {
        $line = $raw.Trim()
        if (-not $line -or $line.StartsWith('#') -or -not $line.Contains('=')) { continue }
        $idx = $line.IndexOf('=')
        $key = $line.Substring(0, $idx).Trim()
        $val = $line.Substring($idx + 1).Trim().Trim('"').Trim("'")
        if ($key) {
            Set-Item -Path ("Env:{0}" -f $key) -Value $val
            $count++
        }
    }
    Write-Log ("已从 .env 载入 {0} 个变量" -f $count)
}

# 自检：只验证流式写入与失败传播，不需要网络 / cargo / .env
function Invoke-SelfTest {
    $host_exe = (Get-Process -Id $PID).Path
    Write-Log '--- 自检 1/2：流式输出（3 行，行间 0.4s，应在运行期间就能看到）---'

    Invoke-Step -Name 'selftest-stream' -Exe $host_exe -Arguments @(
        '-NoProfile', '-Command',
        '1..3 | ForEach-Object { Write-Output ("selftest-line {0} at {1:HH:mm:ss.fff}" -f $_, (Get-Date)); Start-Sleep -Milliseconds 400 }'
    )

    Write-Log '--- 自检 2/2：失败传播（退出码 3，预期抛出并中止）---'
    try {
        Invoke-Step -Name 'selftest-fail' -Exe $host_exe -Arguments @(
            '-NoProfile', '-Command', 'Write-Output "selftest-will-fail"; exit 3'
        )
        Write-Log '自检 FAIL：非零退出码没有被捕获' 'ERROR'
        return 1
    }
    catch {
        Write-Log ("自检 OK：失败已中止流程 -> {0}" -f $_.Exception.Message)
    }

    Write-Log '自检结束：流式透传与失败即停均正常'
    return 0
}

$script:RunStartLocal = Get-Date
Write-Log '================ 每日流程开始 ================'
Write-Log ("仓库: {0}" -f $RepoRoot)
Write-Log ("实时日志: {0}" -f $LogPath)

# 主流程包成函数：内部的 return 只退出本函数，
# 保证最终的 exit 码由返回值决定，而不是被 return 提前跳过。
function Invoke-Main {
try {
    # 上次运行是否留下了「running」面包屑 —— 说明进程被外部杀掉，没走完 catch/finally
    if (Test-Path -LiteralPath $StateFile) {
        $prev = Get-Content -LiteralPath $StateFile -Encoding UTF8
        $prevStatus = ($prev | Where-Object { $_ -like 'status=*' }) -replace '^status=', ''
        if ($prevStatus -eq 'running') {
            $prevStep = ($prev | Where-Object { $_ -like 'step=*' }) -replace '^step=', ''
            $prevUpd  = ($prev | Where-Object { $_ -like 'updated_local=*' }) -replace '^updated_local=', ''
            Write-Log ("上次运行未正常结束：停在步骤 [{0}]，最后活动 {1}" -f $prevStep, $prevUpd) 'WARN'
        }
    }

    if ($SelfTest) {
        $r = Invoke-SelfTest
        Write-RunState -Status 'done' -Step 'selftest'
        Write-Log '================ 自检结束 ================'
        return $r
    }

    # ---- 0. 环境准备 ----
    Set-Location -LiteralPath $RepoRoot

    if (-not (Test-Path $CargoExe)) { throw "找不到 cargo: $CargoExe" }
    if (-not (Test-Path (Join-Path $RepoRoot 'Cargo.toml'))) { throw "当前目录不是 cargo 项目" }

    Import-DotEnv -Path (Join-Path $RepoRoot '.env')

    # pack --protect 必需
    if (-not $env:FACTORIO_DATA_PATH) {
        throw "FACTORIO_DATA_PATH 未设置，pack --protect 无法进行"
    }
    if (-not (Test-Path -LiteralPath $env:FACTORIO_DATA_PATH)) {
        throw ("FACTORIO_DATA_PATH 指向的路径不存在: {0}" -f $env:FACTORIO_DATA_PATH)
    }
    # upload 必需
    if (-not $env:FACTORIO_MOD_PORTAL_KEY) {
        throw "FACTORIO_MOD_PORTAL_KEY 未设置，upload 无法进行"
    }
    Write-Log ("检查通过：cargo={0}，FACTORIO_DATA_PATH 存在，上传密钥已配置" -f (& $CargoExe --version))

    # 记录步骤前 output\ 的既有 zip，用于之后精确识别本次新产物
    $before = @()
    if (Test-Path $OutputDir) {
        $before = @(Get-ChildItem -LiteralPath $OutputDir -Filter '*.zip' -File | Select-Object -ExpandProperty FullName)
    }
    Write-Log ("output\ 现有 zip: {0} 个" -f $before.Count)

    if ($DryRun) {
        Write-Log 'DryRun: 跳过三个实际步骤' 'WARN'
        Write-RunState -Status 'done' -Step 'DryRun'
        Write-Log '================ 每日流程结束（DryRun） ================'
        return 0
    }

    # ---- 1. 翻译 ----
    Invoke-Step -Name 'translate (cargo r -r)' -Exe $CargoExe -Arguments @('r', '-r')

    # ---- 2. 打包 ----
    # 预警：pack 用 UTC 日期命名，同一天（UTC）内再打包会静默覆盖上一个同名包。
    # 典型场景：本地 07:00 = 22:46–23:59 UTC，若当天 UTC 已有人工打包过，本次会覆盖它。
    $utcToday = (Get-Date).ToUniversalTime().ToString('yyyy.MM.dd')
    $pre = Join-Path $OutputDir ("tanvec-ai-cn_{0}.zip" -f $utcToday)
    if (Test-Path -LiteralPath $pre) {
        $preItem = Get-Item -LiteralPath $pre
        Write-Log ("预警：{0} 已存在（{1:N1} MB，{2:yyyy-MM-dd HH:mm:ss} 本地）——pack 会覆盖它，且无法回退" -f `
            $preItem.Name, ($preItem.Length / 1MB), $preItem.LastWriteTime) 'WARN'
    }
    else {
        Write-Log ("本次 pack 目标（UTC 命名）: tanvec-ai-cn_{0}.zip，当前不存在" -f $utcToday)
    }

    Invoke-Step -Name 'pack --protect (cargo r -r pack --protect)' -Exe $CargoExe `
        -Arguments @('r', '-r', 'pack', '--protect')

    # ---- 3. 选包 + 上传 ----
    $zip = Get-ChildItem -LiteralPath $OutputDir -Filter '*.zip' -File |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if (-not $zip) { throw "output\ 下没有任何 zip，无法上传" }

    $isNew = -not ($before -contains $zip.FullName)
    Write-Log ("选中 zip: {0}（{1:N1} MB，写入于本地 {2:yyyy-MM-dd HH:mm:ss} / UTC {3:yyyy-MM-dd HH:mm:ss}，本次新建={4}）" -f `
        $zip.Name, ($zip.Length / 1MB), $zip.LastWriteTime, $zip.LastWriteTime.ToUniversalTime(), $isNew)
    if (-not $isNew) {
        if ($zip.LastWriteTime -gt (Get-Date).AddMinutes(-30)) {
            Write-Log '注意：选中的 zip 命名于本次运行前（pack 覆盖了同名旧包），内容已更新' 'WARN'
        }
        else {
            Write-Log '警告：选中的 zip 既非本次新建、近期也未被改动，pack 可能未产出内容' 'WARN'
        }
    }

    Invoke-Step -Name 'upload (cargo r -r upload)' -Exe $CargoExe `
        -Arguments @('r', '-r', 'upload', '--file', $zip.FullName)

    Write-RunState -Status 'done' -Step 'finished'
    Write-Log '================ 每日流程成功 ================'
    return 0
}
catch {
    Write-Log ("流程中止: {0}" -f $_.Exception.Message) 'ERROR'
    Write-Log '================ 每日流程失败 ================' 'ERROR'
    try { Write-RunState -Status 'failed' -Step 'aborted' } catch { }
    return 1
}
finally {
    # 兜住所有流的冲刷与释放；被硬杀时这里不会执行，面包屑才留得下
    try { $script:LogStream.Flush() } catch { }
    try { $script:LogStream.Dispose() } catch { }
}
}

exit (Invoke-Main)
