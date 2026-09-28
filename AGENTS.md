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

## 命令（构建与联调）

- 后端构建/测试：`cargo build/test --config 'patch.crates-io.dbx-plugin-sdk.path="<工作区>/tmp/sdk-root"'`（SDK 不在 crates.io，固定缓存在 `tmp/sdk-root`，`gitignore` 不进仓库；CLI 升级后重同步；勿改 Cargo.toml 加 path 依赖）
- 打包：`dbx-plugin package .`（当前平台 target；Alpine 下产 musl 版，仅本地验证用）
- 前端联调：`dbx-plugin dev --path . --port 5190`（只绑回环，跨容器连不上是预期限制，用 curl + diagnostics 验证）

## 测试包链路（每次功能完成必走，验证通过才打 tag 发版）

- 一键：`scripts/make-test-pkg.sh [linux|win]`（官方同版本底包换新 `ui/` + 重算 `checksums` → 存 `tmp/` → 同步 NAS `/tmp` 与 Windows 桌面 → NAS 断开安装 `ping`）
- `linux` 包给 NAS docker 的 DBX，`win` 包给 Windows 桌面 DBX；`version` 不动覆盖安装；`tmp/` 已 `gitignore` 不进仓库
- 安装一律走本机脚本（反代 `dbx.conf` 已加 `client_max_body_size 20m`，UI 直传也可）
- 单独覆盖安装（NAS 本机）：`scripts/repack-test.sh <包>`（有活跃连接会拦 `active connections`，脚本先调断开接口）

## 生产与发版

- 生产二进制：NAS 上 `rust:1-bookworm` 容器编 gnu release，产物重组 `.dbxp`（细节见 LRN-20260925-003）

## 代码规范

- 后端依赖：`russh` + `russh-sftp` + `tokio`（SSH 远端模式，B1 已评估通过，纯 Rust 无系统依赖，`ring` 后端兼顾 musl/gnu）；其余不加新 crate
- JSON 单消息 8MB 上限：大文件走分页/分块，勿整包返回
- Secret（密码/私钥）只走宿主 Secret Store，不进内存日志/context/事件
- 新增 Rust 方法配 `#[cfg(test)]` 单元测试（纯函数直测：解析/过滤/路径防护）
- 前端无构建：`ui/` 三文件（`index.html` 结构 + `style.css` + `app.js`，相对路径引用，沙箱可加载）；日志原文插 DOM 前必须 HTML 转义
- **关键信息记到 AGENTS.md 里，不要记到 learnings 条目里**（避免信息分散）

## 任务收尾自动扫尾（task-wrapup）

- 每次任务 git 收尾前必须执行扫尾（`task-wrapup` skill）：未解决问题新建 `TD-*` 到 `learnings/items/`、有价值信息沉淀为 `LRN-*`，再走 git 提交流程

## Git 提交与版本管理

- 提交格式与收尾流程遵循全局 AGENTS.md 第 9 节（中文 commit message 先审核再提交）
- 版本手动维护：`manifest.json` 与 `backend/Cargo.toml` 版本号必须一致，`Cargo.lock` 同步更新（CI `--locked` 校验）
- 发版 = GitHub Release（Tag）：Release published 触发官方 workflow 打 5 平台包 → dbx-store 提候选 PR；Beta 版用 prerelease

## 商店自动更新（2026-09-26 注册，#164 合并后生效）

- 商店侧 `automation/plugin-sources.json` 注册 `yxlongery/dbx-logviewer`（`autoUpdate: true`）；本仓库根 `.dbx-store.json` 为 listing 元数据源（同步器按 tag 取）
- 发版纪律：试水一律 prerelease（同步器只认正式版）；正式版放缓节奏、攒实质变更；**发版前先更新 `releaseNotes` 与 `source` 中的 tag 再打 tag**，tag 打完不动文件
- `.dbx-store.json` 禁令：只用白名单 10 字段（多写报错）；`permissions` 不手写（会被 Release identity 覆盖，历史曾致客户端拒绝更新）；出现的 listing 字段即替换商店现值，须与已审值逐字对齐
- repository 用 `yxlongery`（真实登录名）；`publisher: yxlonger` 已审，不动
- 机制：自动化只开 candidate PR，不签名不合并；误发正式版也只是多一个 PR（细节见 LRN-20260926-002）
