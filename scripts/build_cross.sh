#!/usr/bin/env bash
# Linux 에서 Linux(x86_64·arm64)·Windows 실행 파일을 만든다. 사용: scripts/build_cross.sh
# 필요: rustup 대상 x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, x86_64-pc-windows-gnu,
#       cargo-zigbuild 와 zig, mingw-w64, 설정 앱용 libwebkit2gtk-4.1-dev·libayatana-appindicator3-dev
# 결과: dist/bin/{linux-x86_64,linux-arm64,windows}/
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
# 실행 파일에 빌드한 컴퓨터의 경로가 들어가지 않게 한다
export RUSTFLAGS="--remap-path-prefix=$ROOT=/src --remap-path-prefix=$HOME=/build"
OUT=dist/bin
mkdir -p $OUT/linux-x86_64 $OUT/linux-arm64 $OUT/windows

# 서버. glibc 2.31(Ubuntu 20.04) 이상에서 돌도록 zigbuild 로 만든다
(cd server
 cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.31
 cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.31
 cargo build --release --target x86_64-pc-windows-gnu)
cp server/target/x86_64-unknown-linux-gnu/release/claudy $OUT/linux-x86_64/
cp server/target/aarch64-unknown-linux-gnu/release/claudy $OUT/linux-arm64/
cp server/target/x86_64-pc-windows-gnu/release/claudy.exe $OUT/windows/

# 설정 앱. Linux 판은 이 컴퓨터의 WebKitGTK 로 링크한다(x86_64 만)
(cd manager
 cargo build --release
 cargo build --release --target x86_64-pc-windows-gnu)
cp manager/target/release/ClaudyManager $OUT/linux-x86_64/
cp manager/target/x86_64-pc-windows-gnu/release/ClaudyManager.exe $OUT/windows/
ls -l $OUT/*
