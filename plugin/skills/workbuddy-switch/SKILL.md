---
name: workbuddy-switch
description: WorkBuddy / CodeBuddy 多账号管理与会话迁移。用于：切换各客户端登录账号（换号）、把当前对话导给另一个账号、复制会话、查看账号库与登录态、查积分到期与 Token 用量、签到、派猫猫旅行、CodeBuddy CLI 自动轮换。当用户说「换号」「切换账号」「把这个对话给另一个号」「导过去」「查积分」时使用本技能。
---

# workbuddy-switch

本插件把 WorkBuddy Switch 的账号能力接进 CodeBuddy / WorkBuddy，全部通过 MCP 工具完成，
不需要用户手动启动任何外部程序。

## 工具一览

| 场景 | 工具 |
| --- | --- |
| 查当前登录态 | `wb_status` |
| 查账号库（切换/导出前必做） | `wb_list_accounts` |
| 查某客户端登录态 | `wb_client_status`（`workbuddy` / `codebuddy-cli` / `codebuddy-ide` / `vscode-ext` / `jetbrains`） |
| 切 WorkBuddy 账号 | `wb_switch_account` |
| 切其它客户端账号 | `wb_switch_client` |
| 列当前账号的会话 | `wb_list_sessions` |
| 按 id 复制会话给目标账号 | `wb_copy_sessions` |
| **把当前对话导给另一个账号** | `wb_export_current_conversation`（`client` 选 `workbuddy` 或 `vscode-ext`） |
| 签到 | `wb_checkin_status` / `wb_checkin` |
| 积分 | `wb_credit_expiry` / `wb_credit_stats` |
| Token 用量 | `wb_token_stats` |
| 模型限额台账 | `wb_rate_limits` |
| 运营 | `wb_travel_status` / `wb_rotate_status` / `wb_rotate_run` |
| 后台任务 | `wb_daemon`（`status` 查询 / `stop` 停当前进程 / `disable`、`enable` 持久开关） |
| 完整界面（图表） | `wb_open_webui`（按需拉起本地 Web UI 并打开浏览器） |

## 导出对话的两个目标端

`wb_export_current_conversation` 的 `client` 决定「当前对话」从哪来：

- **`workbuddy`（默认）**：由插件 hook 精确记录当前会话；hook 没生效时回退到最近一条
  带正文的会话（返回里的 `resolvedBy` 会标 `hook` 或 `latest`）。
  可用 `switch=true` 顺带把 WorkBuddy 切到目标账号。
- **`vscode-ext`**：VS Code 里的 CodeBuddy 插件不触发 hook，只能取该插件**最近更新且带正文**
  的那条对话。**不支持 `switch`**（没有「导出并切换」这条组合流程），且要求 VS Code 已完全退出。

## 铁律

签到 / 旅行 / 自动轮换 / 保活 / 限额监听由随客户端自动拉起的**守护进程**执行，
它不随客户端退出（周期任务必须独立存活）。用户说「后台任务」「自动签到」「自动轮换没生效」
时，用 `wb_daemon` 的 `status` / `stop`。

用户说「**把后台任务关掉**」「别自动跑」时，必须用 `action=disable` —— 它写的是**持久配置**
（`~/.wb-switch/daemon_config.json` 的 `backgroundTasks: false`），此后会话不再拉起守护。
只 `stop` 是不够的：下一个会话启动又会被拉起来，用户会以为没关掉。

`disable` 只停周期任务；账号切换、导出对话、查询统计等**按需能力不受影响**。恢复用
`action=enable`（不会立刻拉起，下次会话启动时生效）。

## 铁律

1. **切换/导出前先 `wb_list_accounts` 取账号 `id`**，不要用昵称拼 id。
2. **目标账号不明确时先问用户**。切换会关闭并重开客户端，猜错代价高。
3. **WorkBuddy 运行中不能只复制会话**（数据被 App 缓存在内存，写入会被覆盖）。
   此时用 `wb_export_current_conversation` 的 `switch=true`——切换流程会先关闭 WorkBuddy
   再写入、最后重开，这是唯一安全的做法。
4. **CodeBuddy CLI 切换会中断当前会话且不自动重开**，要提前告知用户。
5. 统计类结果用中文表格汇报，不要贴原始 JSON。
