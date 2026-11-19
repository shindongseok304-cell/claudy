#!/bin/sh
# Claudy 설치 (Linux). 사용: sh install.sh
set -e
here=$(cd "$(dirname "$0")" && pwd)
dest="$HOME/.local/share/claudy"
mkdir -p "$dest" "$HOME/.local/bin" "$HOME/.local/share/applications"

# 서버
install -m 755 "$here/claudy" "$dest/claudy"
ln -sf "$dest/claudy" "$HOME/.local/bin/claudy"
echo "서버 설치: $dest/claudy ($("$dest/claudy" --version))"

# 설정 앱(있을 때만)
if [ -f "$here/ClaudyManager" ]; then
  install -m 755 "$here/ClaudyManager" "$dest/ClaudyManager"
  [ -f "$here/claudy.png" ] && install -m 644 "$here/claudy.png" "$dest/claudy.png"
  cp "$here/MANAGER_VERSION" "$dest/MANAGER_VERSION" 2>/dev/null || true
  cat > "$HOME/.local/share/applications/claudy.desktop" <<DESK
[Desktop Entry]
Type=Application
Name=Claudy
Comment=Claudy 설정
Exec="$dest/ClaudyManager"
Icon=$dest/claudy.png
Terminal=false
Categories=Utility;Development;
DESK
  echo "설정 앱 설치: 프로그램 메뉴의 Claudy"
  missing=""
  for lib in libwebkit2gtk-4.1.so.0 libayatana-appindicator3.so.1; do
    ldconfig -p 2>/dev/null | grep -q "$lib" || missing="$missing $lib"
  done
  [ -n "$missing" ] && echo "설정 앱에 필요한 라이브러리가 없습니다:$missing (Ubuntu·Debian: sudo apt install libwebkit2gtk-4.1-0 libayatana-appindicator3-1)"
fi

# Claude Code 에 등록
if command -v claude >/dev/null 2>&1; then
  claude mcp remove claudy -s user >/dev/null 2>&1 || true
  claude mcp add claudy -s user -- "$dest/claudy" && echo "Claude Code 에 claudy 등록 완료(모든 폴더에서 사용)"
else
  echo "Claude Code 가 없어 등록을 건너뜁니다. 설치 뒤 다음을 실행하세요:"
  echo "  claude mcp add claudy -s user -- $dest/claudy"
fi
echo "설치를 마쳤습니다. 설정은 ~/.claudy 에 저장됩니다."
