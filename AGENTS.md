# tanvec-ai-cn — 仓库约定

异星工厂（Factorio）模组自动汉化工具。翻译管道 → 打包成单个 mod → 上传 Mod Portal。

## 每日自动化（重要）

本机有一个 **Windows 计划任务**在每天 **07:00（本地时间 UTC+8）** 执行完整的发布流程：

| 项目 | 值 |
|---|---|
| 任务名 | `tanvec-ai-cn daily translate-pack-upload` |
| 动作 | `pwsh.exe -NoProfile -ExecutionPolicy Bypass -File scripts\daily-translate.ps1` |
| 触发 | 每天 07:00；**错过不补跑**（`StartWhenAvailable` 未启用） |
| 脚本 | `scripts\daily-translate.ps1`（失败即停、**子进程输出逐行实时写日志**） |
| 日志 | `logs\daily-<yyyy-MM-dd>.log`（本地日期） |
| 实时跟看 | `pwsh scripts\watch-log.ps1 -Follow` |

脚本依次执行三步，任一步非零退出即停止：

```
cargo r -r                       # 1. 翻译（无子命令 = Translate）
cargo r -r pack --protect        # 2. 打包
cargo r -r upload --file <zip>   # 3. 上传
```

第三步的 `--file` **自动取 `output\` 下最近修改的 `.zip`**，不用日期拼名字。

### 时间戳与命名一律是 UTC（历史原因）

本仓库的**文件名、日志时间、`.pipeline_state.last_run` 全部用 UTC+0**，不是本地时间。
机器在 UTC+8，所以本地 07:00 ≈ UTC **前一天** 23:00。核对过的实际对应关系：

| 本地打包时刻 | UTC 打包时刻 | 产物文件名 |
|---|---|---|
| 2026-09-29 06:55 | 2026-09-28 22:55 | `tanvec-ai-cn_2026.09.28.zip` |
| 2026-09-28 07:41 | 2026-09-27 23:41 | `tanvec-ai-cn_2026.09.27.zip` |
| 2026-09-27 06:59 | 2026-09-26 22:59 | `tanvec-ai-cn_2026.09.26.zip` |

**看文件名时不要按本地日期理解**，差一天是正常的。

### 覆盖风险（脚本已加预警）

`pack` 用 **UTC 日期**命名，同一 UTC 日内再打包会**静默覆盖**上一个同名包，无法回退。
触发条件很窄但真实：脚本安排在本地 07:00 = 22:46–23:59 UTC，
若那天 UTC 日期内已有人工打包过，本次会覆盖它，随后 `upload` 又把覆盖后的内容传上去。
`scripts\daily-translate.ps1` 在 pack 前检查同名文件并打 `WARN` 日志。

### 实时日志与运行状态

`daily-translate.ps1` 把子进程输出**逐行写入日志并立即 flush**，所以运行期间就能看：

```powershell
pwsh scripts\watch-log.ps1 -Follow            # 跟今天的日志，流程结束标记处自动退出
pwsh scripts\watch-log.ps1 -Date 2026-10-24 -Tail 100
pwsh scripts\watch-log.ps1 -Follow -Raw       # 无视结束标记，Ctrl+C 退出
```

实现要点（改脚本时别退回旧写法）：

- 日志用 `StreamWriter` + `AutoFlush = $true`，**不要**用 `Out-String` / `Add-Content` 攒批，
  那会让「进程被杀」和「子进程卡死」在日志里变成同一个样子（历史上就是这么失明的）。
- 每步边界写 `logs\.run-state` 面包屑（`status` / `step` / `pid` / 时间 / 日志路径），
  在 `finally` 里收尾。**进程被硬杀时 `finally` 不执行**，面包屑会留在 `status=running`，
  下次运行开头就会报「上次运行未正常结束：停在步骤 [x]，最后活动 <时间>」。
- 自检（不碰 cargo、不联网）：
  `pwsh scripts\daily-translate.ps1 -SelfTest -LogPath $env:TEMP\st.log`

### 运维要点

- 手动改脚本或换 cargo 路径后，用 `scripts\daily-translate.ps1 -DryRun` 先验证环境。
- 立即补跑一次：`schtasks /Run /TN "tanvec-ai-cn daily translate-pack-upload"`
- 停掉正在跑的任务：`schtasks /End /TN "tanvec-ai-cn daily translate-pack-upload"`
  （**注意**：`/End` 只结束脚本进程，`cargo` / `tanvec-ai-cn` 子进程可能残留，
  需要 `Stop-Process -Name cargo,tanvec-ai-cn -Force` 收尾。）
- 翻译管道很快：实测 2026-09-28 那次从 `22:46:18Z` 起、`22:54:53Z` 写完 cache，
  **约 8.5 分钟**（不是数小时）；`pack` / `upload` 各只需秒级。
- 覆盖窗口是精确续接的：`last_run` 取**运行开始**时刻，且仅在自动模式正常跑完才更新，
  所以相邻两次运行的区间不重不漏，只是边界整体平移。
- **本机电源策略**：任务配置为「使用电池时不启动、使用电池时停止」。
  若笔记本会在 07:00 前拔电或进入电池供电，任务会被跳过或中断。
- **已知静默失败**（2026-10-05 / 10-22 / 10-24）：07:00 触发后 1 秒内进程消失，
  日志只有 511 字节、停在「开始: translate」，连 cargo 第一行都没有。系统事件里
  没有关机/重启记录，`Last Result` 仍是 0。任务 XML 里
  `LogonType=InteractiveToken`、`DisallowStartIfOnBatteries=true`、`StopIfGoingOnBatteries=true`。
  旧脚本的缓冲让「被杀」与「卡死」无法区分，因此当时未能定性；改为流式后同类问题
  可直接从日志判断到哪一行，且 `.run-state` 会留下 `running` 面包屑。
- `scripts\` 与 `logs\` 目前**未**纳入 git（`.gitignore` 未涵盖），是否入库按需决定。
  其中 `logs\.run-state` 是运行时状态文件，不该入库。

## 已知坑

- `.env` 必需：`DEEPSEEK_KEY`、`FACTORIO_DATA_PATH`（`pack --protect` 用）、
  `FACTORIO_MOD_PORTAL_KEY`（`upload` 用）、`FACTORIO_USERNAME` / `FACTORIO_PASSWORD`（拉取 mod 用）。
- `cargo r -r` 不带子命令走的是**默认 Translate**，不是打包。
- `pack` 输出路径固定为 `output\<name>_<UTC日期>.zip`，同名会被直接覆盖。
- 管道用 `cache\.pipeline_state` 的 `last_run` 决定增量起点；该文件**仅在自动模式正常跑完**
  时才更新（`src/flow.rs` 末尾），中断运行会让下次重复抓取同一批 mod。
