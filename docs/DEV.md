# 开发者文档（DEV）

本页给贡献者看，用户文档在 [README](../README.md)。

## 目录结构

```text
manifest.json                  插件身份、连接表单、权限与工作台声明
backend/src/main.rs            Rust Sidecar：连接管理 ＋ 日志接口
ui/index.html                  单文件前端，无构建，开箱即用
assets/plugin.svg              插件图标
docs/screenshot.png            工作台截图
.github/workflows/             Release 自动打 5 平台包
```

日志接口：`logs/browse`（目录浏览）· `logs/search`（搜索）· `logs/tail` / `logs/stop`（实时启停）· `logs/downloadChunk`（分块下载）。

## 本地开发

```bash
# 后端构建/测试（SDK 不在 crates.io，用 CLI 自带目录 patch，勿改 Cargo.toml）
cargo build --config 'patch.crates-io.dbx-plugin-sdk.path="<CLI自带sdk-root>/plugins/sdk/rust/dbx-plugin-sdk"'
cargo test --config 'patch.crates-io.dbx-plugin-sdk.path="<CLI自带sdk-root>/plugins/sdk/rust/dbx-plugin-sdk"'

# 前端联调（只绑回环，开发机浏览器打开）
dbx-plugin dev --path . --port 5190

# 打本地验证包（当前平台，Alpine 下为 musl 版，仅本地验证用）
dbx-plugin package .
```

生产二进制在 NAS 上 `rust:1-bookworm` 容器编 gnu release 版后重组 `.dbxp`（Alpine 产 musl，在 Debian 系 DBX 容器里起不来）。

## 发版

打 Tag → GitHub Release（published 触发 workflow 打 5 平台包）→ 向 `t8y2/dbx-store` 提候选 PR（首发 [#164](https://github.com/t8y2/dbx-store/pull/164)）。版本号在 `manifest.json` 与 `backend/Cargo.toml` 手动同步保持一致（`Cargo.lock` 同步更新，CI `--locked` 校验）。
