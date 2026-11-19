#!/usr/bin/env bash
# dist/bin 의 실행 파일로 배포 파일을 묶는다. 사용: scripts/package.sh
# 필요: Node.js(npx @anthropic-ai/mcpb), zip
# 결과: dist/release/Claudy_v<판>_{Windows,Mac}.zip, Claudy_v<판>_Linux_{x86_64,arm64}.tar.gz
set -euo pipefail
cd "$(dirname "$0")/.."
VER=$(grep -m1 '^version' server/Cargo.toml | cut -d'"' -f2)
MVER=$(grep -m1 '^version' manager/Cargo.toml | cut -d'"' -f2)
BIN=dist/bin; REL=dist/release; WORK=dist/work
rm -rf $REL $WORK && mkdir -p $REL $WORK

mcpb() { # <플랫폼 폴더> <manifest> <실행 파일> <결과 mcpb>
  local d=$WORK/mcpb-$1; mkdir -p $d/server
  cp server/mcpb/$2 $d/manifest.json
  cp $BIN/$1/$3 $d/server/$3
  chmod +x $d/server/$3
  npx -y @anthropic-ai/mcpb pack $d "$4" >/dev/null
}

if [[ -f $BIN/windows/claudy.exe ]]; then
  d=$WORK/Claudy_v${VER}_Windows; mkdir -p $d
  mcpb windows manifest.json claudy.exe "$d/claudy-$VER-windows.mcpb"
  cp $BIN/windows/ClaudyManager.exe $d/
  sed "s/VERSION/$VER/" packaging/text/windows.txt > "$d/설치방법.txt"
  (cd $WORK && zip -qr ../release/Claudy_v${VER}_Windows.zip Claudy_v${VER}_Windows)
fi
if [[ -f $BIN/mac/claudy ]]; then
  d=$WORK/Claudy_v${VER}_Mac; mkdir -p $d
  mcpb mac manifest.mac.json claudy "$d/claudy-$VER-mac.mcpb"
  cp -R $BIN/mac/Claudy.app $d/
  sed "s/VERSION/$VER/" packaging/text/mac.txt > "$d/설치방법.txt"
  (cd $WORK && zip -qry ../release/Claudy_v${VER}_Mac.zip Claudy_v${VER}_Mac)
fi
for arch in x86_64 arm64; do
  [[ -f $BIN/linux-$arch/claudy ]] || continue
  n=Claudy_v${VER}_Linux_$arch; d=$WORK/$n; mkdir -p $d
  install -m 755 $BIN/linux-$arch/claudy $d/claudy
  if [[ -f $BIN/linux-$arch/ClaudyManager ]]; then
    install -m 755 $BIN/linux-$arch/ClaudyManager $d/ClaudyManager
    cp packaging/linux/claudy.png $d/
    echo "$MVER" > $d/MANAGER_VERSION
  fi
  install -m 755 packaging/linux/install.sh packaging/linux/uninstall.sh $d/
  cp packaging/text/linux.txt "$d/설치방법.txt"
  tar -C $WORK -czf $REL/$n.tar.gz $n
done
ls -l $REL
