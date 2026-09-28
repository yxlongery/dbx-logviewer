#!/bin/bash
# 一键覆盖安装测试包：先断开活跃连接（否则报 Plugin update blocked by active connections），再装包并 ping 验证
# 用法（NAS 本机）：scripts/repack-test.sh /tmp/lv-linux-test.dbxp ["连接名，默认 Log Viewer connection"]
set -e
PKG=${1:?用法：repack-test.sh <包路径> [连接名]}
NAME=${2:-Log Viewer connection}
BASE=http://127.0.0.1:4224
JAR=$(mktemp)
trap 'rm -f $JAR' EXIT
P=$(grep DBX_PASSWORD /vol1/1000/docker/dbx/dbx.env 2>/dev/null | cut -d= -f2)
[ -z "$P" ] && P=$(grep -rh DBX_PASSWORD /vol1/1000/docker/dbx/ 2>/dev/null | head -1 | cut -d= -f2)
curl -s -c "$JAR" -X POST "$BASE/api/auth/login" -H 'Content-Type: application/json' -d "{\"password\":\"$P\"}" -o /dev/null
# 断开：按名查 id（无匹配则跳过，不断开就强装会失败，set -e 直接停）
ID=$(curl -s -b "$JAR" "$BASE/api/connection/list" | python3 -c "import json,sys; print(next((c['id'] for c in json.load(sys.stdin) if c.get('name')=='$NAME'), ''))")
if [ -n "$ID" ]; then
  curl -s -b "$JAR" -X POST "$BASE/api/connection/disconnect" -H 'Content-Type: application/json' -d "{\"connectionId\":\"$ID\"}" -o /dev/null
  echo "已断开：$NAME"
fi
curl -s -b "$JAR" -F file=@"$PKG" "$BASE/api/plugins/install?allow_unsigned=true" | head -c 200; echo
curl -s -b "$JAR" -X POST "$BASE/api/plugins/invoke" -H 'Content-Type: application/json' -d '{"pluginId":"io.github.yxlonger.logviewer","method":"dbx-logviewer/ping","params":{}}'; echo
