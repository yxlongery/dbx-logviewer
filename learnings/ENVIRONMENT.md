# ENVIRONMENT.md

本项目的构建与 DBX 环境清单。**本机通用环境（工具链/代理/NAS 通用信息）见全局 `~/.config/opencode/learnings/ENVIRONMENT.md`（单一来源），先看那边**。基础设施变更时同步更新本文件。

## gnu 构建（NAS）

- 构建容器：`rust:1-bookworm`（NAS 已拉取），挂载源码 + SDK + 缓存：
  - 源码：`/tmp/logviewer-build/backend`（从 workspace 拷贝，旧路径副本用前重拷）
  - SDK：`/tmp/dbx-sdk/sdk-root`（从 CLI 自带目录解压）
  - 缓存：`/tmp/cargo-home`（`CARGO_HOME`，复用 registry）
  - patch 配置：`/tmp/logviewer-build/cfg/config.toml` 挂到 `$CARGO_HOME/config.toml`
- 产物：`backend/target/release/dbx-plugin-dbx-logviewer`（gnu，interpreter `/lib64/ld-linux-x86-64.so.2`）
- 重组 `.dbxp` 在 opencode 容器用 python3 zipfile（无 zip 命令），同步更新 `checksums.json` + `artifact.json`，manifest 内 `executable` 须为 `bin/linux-x64/...` 形态

## DBX 环境（NAS）

- 容器：`dbx`（`t8y2/dbx:latest`，Debian 12），端口宿主 `127.0.0.1:4224`（经反代对外）
- 数据卷：宿主 `/vol1/1000/docker/dbx/data` → 容器 `/app/data`；插件落盘 `data/plugins/<id>/`
- 密码：宿主 `dbx.env` 的 `DBX_PASSWORD`（脱敏，用时现查，不落本文件）
- sshd 禁 TCP 转发：操作走「ssh 进 NAS 本机 curl」，不建 `-L` 隧道
- 测试日志：宿主 `/vol1/1000/docker/dbx/data/logtest`（容器内 `/app/data/logtest`）
- 插件调试：`POST /api/plugins/invoke` 直调 sidecar；未签名包 `POST /api/plugins/install?allow_unsigned=true`

## 上架链路

- 源码：`github.com/yxlongery/dbx-logviewer`；商店 PR：`t8y2/dbx-store#164`
- 发版：打 Tag → GitHub Release（prerelease 试水）→ 官方复用 workflow（ref 用 `@main`）打 5 平台包 → 提候选 PR
- 本文件不落密钥/token/私钥，只写脱敏摘要

## Git 远端（2026-09-25 切 ssh）

- `origin` 为 `ssh` 形态（宿主 GitHub 独立 key 直推）；容器内无私钥，凡访问远端的操作（fetch/pull/ls-remote/push）会失败，本地操作正常
- `push` 固定走宿主：`ssh nas 'cd /vol1/1000/docker/opencode/data/workspace/dbx-logviewer && git push origin main'`
- 宿主侧曾报 `dubious ownership`，已加 `safe.directory` 例外（宿主本地 git config，不进提交）
- 容器内 GitHub key 已配（2026-09-25）：私钥 `id_ed25519_github` 在持久卷 ssh 目录、`config` 有 `Host github.com` 段；容器内远端操作加 `GIT_SSH_COMMAND="ssh -F /root/.local/share/opencode/ssh/config"` 前缀即可直推，无需再走宿主
