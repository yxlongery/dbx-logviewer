#!/bin/bash
# 一键测试包：重组 → 存 tmp/ → scp 桌面 →（linux）NAS 断开+安装+ping
# 用法（本容器）：scripts/make-test-pkg.sh [linux|win] [--local-bin]，默认 linux
# 口径：官方同版本底包为底；--local-bin 用 NAS 现编二进制+工作区 manifest（后端/表单变了时用），
#   否则只换 ui/（纯前端改动，免重编）；version 不动覆盖安装；tmp/ 已 gitignore
set -e
cd "$(dirname "$0")/.."
PLAT=${1:-linux}
[ "$2" = "--local-bin" ] && LOCAL_BIN=1 || LOCAL_BIN=0
STAMP=$(date +%Y%m%d-%H%M)
BASE=$([ "$PLAT" = win ] && echo lv-win.dbxp || echo lv-linux.dbxp)
EXE=$([ "$PLAT" = win ] && echo "bin/windows-x64/dbx-plugin-dbx-logviewer.exe" || echo "bin/linux-x64/dbx-plugin-dbx-logviewer")
OUT=tmp/lv-${PLAT}-test-${STAMP}.dbxp
mkdir -p tmp
[ -f /tmp/"$BASE" ] || scp "nas:/tmp/$BASE" /tmp/"$BASE"
if [ "$LOCAL_BIN" = 1 ]; then
  ssh nas "rm -rf /tmp/logviewer-build/backend/src && cp -r /vol1/1000/docker/opencode/data/workspace/dbx-logviewer/backend/src /tmp/logviewer-build/backend/src && cp /vol1/1000/docker/opencode/data/workspace/dbx-logviewer/backend/Cargo.toml /vol1/1000/docker/opencode/data/workspace/dbx-logviewer/backend/Cargo.lock /tmp/logviewer-build/backend/ && docker run --rm --name lvbuild -v /tmp/logviewer-build/backend:/src -v /tmp/dbx-sdk/sdk-root:/sdk -v /tmp/cargo-home:/cargo -v /tmp/logviewer-build/cfg/config.toml:/cargo/config.toml -e CARGO_HOME=/cargo -w /src rust:1-bookworm cargo build --release 2>&1 | tail -2"
  scp "nas:/tmp/logviewer-build/backend/target/release/dbx-plugin-dbx-logviewer" /tmp/lv-newbin
fi
LOCAL_BIN=$LOCAL_BIN python3 - "$BASE" "$OUT" "$EXE" <<'EOF'
import zipfile, json, hashlib, sys, os
local = os.environ.get("LOCAL_BIN") == "1"
src = zipfile.ZipFile("/tmp/" + sys.argv[1])
exe, out = sys.argv[3], sys.argv[2]
rep = {}
for fn in ["index.html", "style.css", "app.js"]:
    rep["ui/" + fn] = open("ui/" + fn, "rb").read()
if local:
    rep[exe] = open("/tmp/lv-newbin", "rb").read()
    mf = json.load(open("manifest.json"))
    mf["entrypoints"]["backend"]["executable"] = exe
    rep["manifest.json"] = json.dumps(mf, indent=2, ensure_ascii=False).encode()
chk = {"algorithm": "sha256", "files": {}}
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    for n in src.namelist():
        if n == "checksums.json":
            continue
        data = rep.get(n, src.read(n))
        z.writestr(n, data)
        chk["files"][n] = hashlib.sha256(data).hexdigest()
    for n in ["ui/style.css", "ui/app.js"]:
        if n not in chk["files"]:
            z.writestr(n, rep[n])
            chk["files"][n] = hashlib.sha256(rep[n]).hexdigest()
    z.writestr("checksums.json", json.dumps(chk, indent=2))
z = zipfile.ZipFile(out)
chk2 = json.loads(z.read("checksums.json"))
assert all(hashlib.sha256(z.read(f)).hexdigest() == h for f, h in chk2["files"].items() if f != "checksums.json"), "checksums MISMATCH"
print("repacked:", out)
EOF
rm -f /tmp/lv-newbin
scp "$OUT" "nas:/tmp/lv-${PLAT}-test.dbxp"
scp "$OUT" "windows:C:/Users/yxlonger/Desktop/lv-${PLAT}-test.dbxp"
echo "桌面已同步：lv-${PLAT}-test.dbxp"
if [ "$PLAT" = linux ]; then
  ssh nas "bash /vol1/1000/docker/opencode/data/workspace/dbx-logviewer/scripts/repack-test.sh /tmp/lv-${PLAT}-test.dbxp"
fi
