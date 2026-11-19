#!/bin/zsh
# macOS(Apple Silicon)에서 서버와 설정 앱을 만든다. 사용: scripts/build_mac.sh
# CLAUDY_SIGN 에 서명 스크립트(인자: <파일|앱> <식별자>)를 주면 그것으로 서명하고, 없으면 ad-hoc 서명한다
# 결과: dist/bin/mac/claudy, dist/bin/mac/Claudy.app
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
export RUSTFLAGS="--remap-path-prefix=$ROOT=/src --remap-path-prefix=$HOME=/build"
OUT=dist/bin/mac
rm -rf $OUT && mkdir -p $OUT
(cd server && cargo build --release)
cp server/target/release/claudy $OUT/claudy
(cd manager && zsh tools/build_mac.sh)
cp -R manager/dist/Claudy.app $OUT/Claudy.app
if [[ -n "${CLAUDY_SIGN:-}" ]]; then
  "$CLAUDY_SIGN" $OUT/claudy com.claudy.server
  "$CLAUDY_SIGN" $OUT/Claudy.app com.claudy.manager
else
  codesign --force --sign - --identifier com.claudy.server $OUT/claudy
fi
ls -l $OUT
