---
description: 把当前对话导给另一个账号（副本写进目标账号的会话库，源账号数据不变）
argument-hint: "[目标账号昵称或邮箱]"
---

把**当前对话**导出给另一个账号。

## 步骤

1. 先调用 `wb_list_accounts` 拿到账号列表（含 `id`、昵称/邮箱）。这步必做——`wb_export_current_conversation` 需要账号 `id`，不能靠昵称猜。
2. 判断目标账号：
   - 用户已指明（昵称或邮箱）→ 在列表里匹配出对应 `id`；
   - 未指明 → 列出候选账号让用户选，**不要随便挑一个**。
3. 调用 `wb_export_current_conversation`，参数：
   - `target_account_id`：上一步得到的 `id`
   - `client`：默认 `workbuddy`（从 WorkBuddy 取当前对话）。若用户明确说的是 VS Code 里的
     CodeBuddy 插件，用 `vscode-ext`
   - `switch`：默认不传。**仅 `client=workbuddy` 时可用**；只有当 WorkBuddy 正在运行、
     或用户希望顺带切过去时才传 `true`。`client=vscode-ext` 时禁止传（会被拒绝）
4. 汇报结果：说明导出的是哪个会话、给了哪个账号、以及 `resolvedBy` 字段的含义（`hook` = 由客户端 hook 精确记录；`latest` = 回退取的最近一条会话——如果是这个，提醒用户确认导出的确实是想导的那条）。

## 关键约束（务必如实转述，不要含糊）

- **WorkBuddy 运行中时不能只复制**：会话数据被 App 缓存在内存里，直接写会被覆盖，所以这种调用会被拒绝。此时要么让用户先退出 WorkBuddy，要么用 `switch=true`——切换流程会自己先关闭 WorkBuddy、写完再重开。
- **`client=vscode-ext` 时要求 VS Code 已完全退出**（同样是因为运行中的扩展会覆盖写入），且**不支持** `switch=true`。
- **源账号数据不变**：副本以新会话 id 写进目标账号，源账号完全不受影响。
- 导出不会自动切换账号。用 `switch=true` 才会把 WorkBuddy 切到目标账号。

不要输出原始 JSON；用中文说明结果。失败时直接说明原因，不要重试多次。
