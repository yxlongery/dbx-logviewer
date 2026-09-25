---
name: task-wrapup
description: 任务结束收尾时自动扫尾：把本轮会话的未解决问题新建为 learnings/items/ 的 TD 条目、有价值信息沉淀为 LRN 条目，并随 git 一起提交。Use ONLY when 一次任务主体已完成、准备收尾 git 前，或用户说「扫尾/记录未解决问题/总结下这些经验」「还有没有该记的」。不要用于实际功能开发执行阶段。
---

# 任务收尾自动扫尾

**扫尾前必须先执行 `engineering-practices` skill**，检查本轮会话是否遵守了工程实践规范。

每次任务的最后一个步骤（全局 AGENTS.md 第 9 节 git 收尾之前）自动执行扫尾：扫描**本轮会话**（或按需交叉核对 opencode.db 的其它历史会话），把两类内容沉淀下来再走 git 流程。

## 判断标准

### A. 未解决问题 → 新建 `TD-*` 条目
满足任一即记录：
- 会话中提出但未解决/被中途放弃（"以后再说"、"新开会话再解决"、"先记下来"）的事项
- 已知但未根治的 bug、已知局限、待确认项（如 NAS 侧配置、待回验）
- 明显半途中断的任务（无收尾提交/无"已完成"结论）
- 明确推迟的计划（将来再做：dev 分支、CI/CD、新标签流程等）

不记录：已完成并提交的任务；用户明确放弃（"不要了"）的想法；执行中的杂碎细节。

### B. 有价值信息 → 新建 `LRN-*` 条目
按 self-improvement skill 的格式与分类（correction/insight/gotcha/best_practice 等），记录：
- 工具/命令踩坑（含本机环境坑）、用户纠正过的认知
- 环境拓扑与个人信息（NAS 地址、G 盘直连、脚本约定等，脱敏后记 ENVIRONMENT.md）
- 项目特定约定（写 AGENTS.md）、通用经验（写全局 `~/.config/opencode/learnings/items/`）

## 条目文件规范

位置：项目 `learnings/items/`（全局通用经验在 `~/.config/opencode/learnings/items/`）。**一文件一条目**，文件名 = ID：

```
learnings/items/TD-20260826-001.md
---
id: "TD-20260826-001"
type: td                # lrn|td
category: "task"
status: "open"          # open 未解决 | watching 观察项 | done 已完成/已归档 | dropped 已放弃
status-detail: "..."    # 原 Status 行全文（如「已完成（提交 abc123）」），可省略
priority: "medium"
area: ["av"]
aliases: ["中文标题"]
created: 2026-08-26
---
# 中文标题

- **来源**：会话名 + 日期
- **问题/计划**：一句话描述
- **可选解 / 计划内容**（如有）
- **验证口径**：明确的完成判据（如 `ssh nas '...'` 有输出）
```

**编号规则**（自旧 TODO.md 时代延续）：
- 编号 = `TD-YYYYMMDD-XXX`，日期 = 条目创建日，XXX 当日序号；全局递增、不回收、跨会话稳定，供 AGENTS / learnings / 提交信息交叉引用
- 新条目编号 = 当前最大 TD 编号 + 1（日期切换时从 `-001` 重新起）；用 `ls learnings/items/ | grep TD-` 核对当日已有序号
- 一次性验证 / 部署回验 / 具体小修复属辅助记录，编号用 `TD-YYYYMMDD-A<nn>`（A 系列，不占正式序号）
- **状态流转不改文件名**：条目完成/放弃时只改 frontmatter `status`（→ done/dropped）并更新 `status-detail`（附提交号）

LRN 条目同构（type: lrn，category 取 correction/insight/gotcha/best_practice 等），格式模板见 self-improvement skill。条目间关联用 wiki-link（`[[LRN-xxx]]`）写在正文尾部 `## 相关条目` 小节；AGENTS/skill 中引用一律纯 ID 文本。

## 执行流程

1. 主体任务完成 → 先做扫尾检查，不直接进入 git 提交
2. 扫 `learnings/items/` 的 TD 条目（可用 `rg "^status: \"(open|watching)\"" learnings/items/ -l`）：本轮内容命中未解决问题 → 新建 TD 条目；已解决项 → 改其 status 为 done 并补 status-detail
3. 扫 `learnings/items/` 近期 LRN 与 `learnings/ENVIRONMENT.md`：判断是否有新经验需沉淀
4. 需要落盘 → 用 Write 新建条目文件（或 Edit 更新既有条目的 status）
5. 拟定 commit message（docs 类型）供用户审核 → 通过后 add/commit（push 按全局约定）
6. 未发现任何需记录的内容 → 跳过并说明

## 注意

- 本项目版本手动维护（manifest.json + backend/Cargo.toml），纯文档提交不触版本，无需顾虑影响 tag
- 不要为确定已完成的事项硬造条目；宁可少记，不可误记
- `learnings/ENVIRONMENT.md` 不落密钥/token/私钥，只写脱敏摘要
