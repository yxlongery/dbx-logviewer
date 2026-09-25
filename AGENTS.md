# AGENTS.md

## 会话启动加载（全局 AGENTS 第 0 节的项目侧部分）

进入本项目工作后，在全局知识源之外一并加载：

### 加载项目 Skill（2 个）

1. `project-knowledge` — 项目知识库（结构/RPC/构建链/learnings 路由）
2. `task-wrapup` — 任务收尾扫尾

### 加载项目文档（2 个）

1. `learnings/ENVIRONMENT.md` — 项目构建与 DBX 环境
2. `AGENTS.md` — 项目 AGENTS（本文件）

## 项目概览

DBX 日志查看器插件：Rust Sidecar + 单文件前端，在服务器上看 `.log` 日志，支持滚动查询、模糊搜索、实时监控、下载。源码仓库 `yxlongery/dbx-logviewer`，商店 PR `t8y2/dbx-store#164`。

## 命令

- 后端构建/测试：`cargo build/test --config 'patch.crates-io.dbx-plugin-sdk.path="<CLI自带sdk-root>/plugins/sdk/rust/dbx-plugin-sdk"'`（SDK 不在 crates.io，勿改 Cargo.toml 加 path 依赖）
- 打包：`dbx-plugin package .`（当前平台 target；Alpine 下产 musl 版，仅本地验证用）
- 前端联调：`dbx-plugin dev --path . --port 5190`（只绑回环，跨容器连不上是预期限制，用 curl + diagnostics 验证）
- 生产二进制：NAS 上 `rust:1-bookworm` 容器编 gnu release，产物重组 `.dbxp`（细节见 LRN-20260925-003）

## 代码规范

- 后端纯 `std` 零新依赖：SSH 这类需新 crate 的能力单独评估，不默默加依赖
- JSON 单消息 8MB 上限：大文件走分页/分块，勿整包返回
- Secret（密码/私钥）只走宿主 Secret Store，不进内存日志/context/事件
- 新增 Rust 方法配 `#[cfg(test)]` 单元测试（纯函数直测：解析/过滤/路径防护）
- 前端无构建：单文件 `ui/index.html`，Vite 规范不适用；日志原文插 DOM 前必须 HTML 转义
- **关键信息记到 AGENTS.md 里，不要记到 learnings 条目里**（避免信息分散）

## 任务收尾自动扫尾（task-wrapup）

- 每次任务 git 收尾前必须执行扫尾（`task-wrapup` skill）：未解决问题新建 `TD-*` 到 `learnings/items/`、有价值信息沉淀为 `LRN-*`，再走 git 提交流程

## Git 提交与版本管理

- 提交格式与收尾流程遵循全局 AGENTS.md 第 9 节（中文 commit message 先审核再提交）
- 版本手动维护：`manifest.json` 与 `backend/Cargo.toml` 版本号必须一致，`Cargo.lock` 同步更新（CI `--locked` 校验）
- 发版 = GitHub Release（Tag）：Release published 触发官方 workflow 打 5 平台包 → dbx-store 提候选 PR；Beta 版用 prerelease
