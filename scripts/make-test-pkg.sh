#!/bin/bash
# 一键测试包：重组 → 存 tmp/ → scp 桌面 →（linux）NAS 断开+安装+ping
# 用法（本容器）：scripts/make-test-pkg.sh [linux|win，默认 linux]
# 口径：官方同版本底包换新 ui/，version 不动覆盖安装；tmp/ 已 gitignore
set -e
cd "$(dirname "$0")/.."
PLAT=${1:-linux}
VER=$(python3 -c "import json; print(json.load(open('manifest.json'))['version'])")
STAMP=$(date +%Y%m%d-%H%M)
BASE=$([ "$PLAT" = win ] && echo lv-win.dbxp || echo lv-linux.dbxp)
OUT=tmp/lv-${PLAT}-test-${STAMP}.dbxp
mkdir -p tmp
[ -f /tmp/"$BASE" ] || scp "nas:/tmp/$BASE" /tmp/"$BASE"
python3 - "$BASE" "$OUT" <<'EOF'
import zipfile, json, hashlib, sys
src = zipfile.ZipFile("/tmp/" + sys.argv[1])
newui = open("ui/index.html", "rb").read()
chk = json.loads(src.read("checksums.json"))
chk["files"]["ui/index.html"] = hashlib.sha256(newui).hexdigest()
with zipfile.ZipFile(sys.argv[2], "w", zipfile.ZIP_DEFLATED) as out:
    for n in src.namelist():
        out.writestr(n, newui if n == "ui/index.html" else (json.dumps(chk, indent=2) if n == "checksums.json" else src.read(n)))
z = zipfile.ZipFile(sys.argv[2])
chk2 = json.loads(z.read("checksums.json"))
assert all(hashlib.sha256(z.read(f)).hexdigest() == h for f, h in chk2["files"].items()), "checksums MISMATCH"
print("repacked:", sys.argv[2])
EOF
scp "$OUT" "nas:/tmp/lv-${PLAT}-test.dbxp"
scp "$OUT" "windows:C:/Users/yxlonger/Desktop/lv-${PLAT}-test.dbxp"
echo "桌面已同步：lv-${PLAT}-test.dbxp"
if [ "$PLAT" = linux ]; then
  ssh nas "bash /vol1/1000/docker/opencode/data/workspace/dbx-logviewer/scripts/repack-test.sh /tmp/lv-${PLAT}-test.dbxp"
fi
