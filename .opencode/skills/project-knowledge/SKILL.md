---
name: project-knowledge
description: dbx-logviewer 项目知识快速入口。Use when 需要查本插件目录结构、Sidecar RPC 方法、musl/gnu 构建链、商店上架流程，或需要读 learnings（ENVIRONMENT/items 条目库）的项目经验、环境明细、未决任务时加载。不要用于任务收尾（用 task-wrapup）或通用学习记录（用 self-improvement）。
---

# 项目知识库（project-knowledge）

AGENTS.md 只保留铁律与高频命令，本 skill 收纳「参考型知识」。遇到本文件列出的场景先加载本 skill，再按需用 Read / Grep 取细节。

## 知识路由（learnings）

- **`learnings/ENVIRONMENT.md`**（NAS 构建与 DBX 环境清单，可整读）：gnu 构建容器、DBX 容器信息、上架链路、测试日志位置。**本机通用环境（工具链/代理/NAS 通用信息）在全局 `~/.config/opencode/learnings/ENVIRONMENT.md`（单一来源），本文件只留项目细节并指向它**
- **`learnings/items/`**：原子化条目库。一文件一条目：LRN=经验教训、TD=任务/观察项；frontmatter 含 id/type/category/status/priority/tags/aliases。定位方式：按 ID 直接 Read，或 `rg "关键词" learnings/items/`；活跃任务用 `rg -l "^status: \"(open|watching)\"" learnings/items/`
- 引用频率高的 LRN：LRN-20260925-001（Alpine 下 plugin-cli 可选包）、LRN-20260925-002（Rust 开发三坑）、LRN-20260925-003（musl/gnu）、LRN-20260925-004（NAS 调试链）、LRN-20260925-005（上架三坑）

**写入职责分工**：新增经验 → `self-improvement` skill（先放项目 `learnings/items/`，通用才回全局）；任务收尾扫尾 → `task-wrapup` skill（项目）。本 skill 只做读取路由，不写内容，避免双份维护。

## 项目结构

- `manifest.json`：插件身份、本地目录连接表单、权限（host.storage/host.events）、workbench 声明
- `backend/src/main.rs`：Rust Sidecar 单文件。7 个 RPC：connection/test/connect/disconnect、logs/list/search/tail+stop/downloadChunk
- `ui/index.html`：单文件前端，无构建。三段式：文件卡片 → 搜索条件 → 日志区
- `dist/`：构建输出不提交（gitignore），发版靠 GitHub Release workflow 产物
- `.dbx-store.json`：无，商店元数据在 PR 里

## RPC 方法速查

- `logs/list`：列目录 `.log/.out`，返回 name/size/modified_at
- `logs/search`：关键字模糊 + 级别 + 时间范围 + 分页（上限 500/页，总量上限 20000 行截断）
- `logs/tail`：起后台线程轮询增量，经 `logs/append` 事件推送，返回 streamId；`logs/stop` 停止
- `logs/downloadChunk`：按行分块，前端循环拼装经 fileTransfer 落盘

## 构建链

- 本地验证（Alpine 容器）：`cargo build/test` + `--config patch` 指自带 SDK（`dbx-plugin package` 会自己处理，勿改 Cargo.toml）
- 生产二进制（Debian 系 DBX 容器）：NAS 上 `rust:1-bookworm` 容器编 gnu 版，产物重组 `.dbxp`（见 LRN-20260925-003）
- 发版：打 Tag → GitHub Release → 官方复用 workflow 打 5 平台包 → dbx-store 提候选 PR（见 LRN-20260925-005）
