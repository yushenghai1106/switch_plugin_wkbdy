---
description: 切换某个客户端的登录账号（WorkBuddy / CodeBuddy CLI / IDE / VS Code 插件 / JetBrains）
argument-hint: "[目标账号昵称或邮箱] [客户端]"
---

把指定客户端切换到另一个账号。

## 步骤

1. 调用 `wb_list_accounts` 取账号列表（`id`、昵称/邮箱）。`wb_switch_account` / `wb_switch_client` 都要账号 `id`。
2. 确认目标客户端：
   - **WorkBuddy 主客户端** → `wb_switch_account`
   - **CodeBuddy CLI / CodeBuddy IDE / VS Code 插件 / JetBrains 插件** → `wb_switch_client`，`client` 取 `codebuddy-cli` / `codebuddy-ide` / `vscode-ext` / `jetbrains`
   - 用户没说 → 问清楚再切，切换会关进程，不要猜。
3. 调用对应工具。需要顺带把当前对话带过去时，用 `wb_switch_account` 的 `copy_session_ids`（值来自 `wb_list_sessions` 的会话 `id`）；只是想导一条对话的话，用 `/wb-switch:export-conversation` 更直接。
4. 汇报：切到了哪个账号、切的是哪个客户端、是否需要重启客户端生效。

## 关键约束（务必如实转述）

- **切换会关闭再重开客户端**。CodeBuddy CLI 的当前会话会中断且**不会**自动重开——这点要提前说清楚，别让用户以为没生效。
- `restart` 默认为 `true`（切完自动重开）。传 `false` 时只写登录态、不重开，此时**不能**同时要求复制会话（复制必须在客户端停止写入之后进行）。
- 只影响被指定的那一个客户端，其它端不受影响。

不要输出原始 JSON；用中文说明结果。
