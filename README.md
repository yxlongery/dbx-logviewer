# 日志查看器 Log Viewer

[![Release](https://img.shields.io/github/v/release/yxlongery/dbx-logviewer?include_prereleases)](https://github.com/yxlongery/dbx-logviewer/releases)
[![License](https://img.shields.io/github/license/yxlongery/dbx-logviewer)](LICENSE)
![DBX](https://img.shields.io/badge/dbx-%3E%3D0.5.68-blue)

> 在 DBX 服务器上直接看 `.log` 日志：滚动查询、模糊搜索、实时监控、分块下载，开箱即用。

![工作台截图](docs/screenshot.png)

## ✨ 功能特性

- 📁 **文件卡片**：自动列出日志目录下的 `.log` / `.out`，显示大小与修改时间，点卡片即查
- 🔗 **链接外部日志**：把目录外的文件按绝对路径链接进来，统一查看（`logs/link`）
- 🗑️ **删除**：链接只删入口（目标保留），普通文件真删，操作前二次确认（`logs/delete`）
- 🔍 **搜索**：关键字模糊 ＋ 级别（ALL / ERROR / WARN / INFO / DEBUG / TRACE）＋ 时间范围 ＋ 分页 ＋ 正序/倒序；搜索条件自动记住，下次打开沿用（`logs/search`）
- ⏱ **实时监控**：从末尾 200 行起播，后台轮询增量经 `logs/append` 事件推送，支持自动滚动，一键启停（`logs/tail` / `logs/stop`）
- ⬇ **下载**：按 2000 行分块拉取、前端拼装后经 fileTransfer 落盘，Web 宿主自动回退 Blob 下载（`logs/downloadChunk`）
- 🛡️ **安全**：日志原文插 DOM 前 HTML 转义；文件名做目录穿越拦截；敏感系统路径拒绝链接

## 🚀 快速上手

1. 在 DBX 插件商店安装本插件（或从 [Releases](https://github.com/yxlongery/dbx-logviewer/releases) 下载 `.dbxp` 手动安装）
2. 新建「日志查看器连接」，填写日志目录（如 `/var/log/aiban/`），测试连通
3. 从该连接进入工作台 → 选文件 → 搜索 / 实时监控 / 下载

## 🖥️ 界面说明

三段式，一屏走完常用流程：

1. **选择日志文件**：文件卡片（含链接入口与删除按钮）＋ 链接表单 ＋ 刷新
2. **搜索条件**：关键字、级别、每页行数、起止时间、排序 ＋ 搜索/下载/实时监控开关
3. **日志区**：暗色等宽渲染，级别着色（ERROR 红 / WARN 黄），关键字高亮，底部分页

## ⚙️ 限制

- 只列出 `.log` / `.out` 文件
- 单页 ≤ 500 行（默认 100）；单次搜索最多返回 20000 行，超出截断并提示
- 实时监控初始 200 行；下载分块 2000 行/轮
- 删除普通文件不可恢复，请谨慎操作

## 🧩 目录结构

```text
manifest.json                  插件身份、连接表单、权限与工作台声明
backend/src/main.rs            Rust Sidecar：连接管理 ＋ 7 个日志接口
ui/index.html                  单文件前端，无构建，开箱即用
assets/plugin.svg              插件图标
docs/screenshot.png            工作台截图
.github/workflows/             Release 自动打 5 平台包
```

日志接口：`logs/list`（列目录）· `logs/search`（搜索）· `logs/tail` / `logs/stop`（实时启停）· `logs/downloadChunk`（分块下载）· `logs/link`（链接外部文件）· `logs/delete`（删除）。

## 🛠️ 本地开发

```bash
# 后端构建/测试（SDK 不在 crates.io，用 CLI 自带目录 patch，勿改 Cargo.toml）
cargo build --config 'patch.crates-io.dbx-plugin-sdk.path="<CLI自带sdk-root>/plugins/sdk/rust/dbx-plugin-sdk"'
cargo test --config 'patch.crates-io.dbx-plugin-sdk.path="<CLI自带sdk-root>/plugins/sdk/rust/dbx-plugin-sdk"'

# 前端联调（只绑回环，开发机浏览器打开）
dbx-plugin dev --path . --port 5190

# 打本地验证包（当前平台，Alpine 下为 musl 版）
dbx-plugin package .
```

生产二进制在 NAS 上 `rust:1-bookworm` 容器编 gnu release 版后重组 `.dbxp`。

## 📦 发版

打 Tag → GitHub Release（published 触发 workflow 打 5 平台包）→ 向 `t8y2/dbx-store` 提候选 PR（首发 [#164](https://github.com/t8y2/dbx-store/pull/164)）。版本号在 `manifest.json` 与 `backend/Cargo.toml` 手动同步保持一致。

## 📄 许可证

[Apache-2.0](LICENSE)。

---

## English Summary

**Log Viewer** is a DBX plugin for browsing server `.log` files: paged search (keyword + level + time range + sort), live tailing via `logs/append` events, chunked download, plus link/delete for files outside the log directory.

- Install from the DBX store (or a `.dbxp` in [Releases](https://github.com/yxlongery/dbx-logviewer/releases)), create a connection with your log directory (e.g. `/var/log/aiban/`), then open the workbench from that connection.
- Limits: lists `.log` / `.out` only; ≤ 500 rows/page (default 100); at most 20000 matched rows per search (truncated with notice); tail starts from the last 200 lines.
- Layout: Rust sidecar (`backend/src/main.rs`) + single-file frontend (`ui/index.html`, no build). Build with `dbx-plugin package .`; releases ship 5-platform binaries via the official reusable workflow.
- License: [Apache-2.0](LICENSE).
