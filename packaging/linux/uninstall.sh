#!/bin/sh
# Claudy 제거 (Linux). 설정 폴더 ~/.claudy 는 남긴다
command -v claude >/dev/null 2>&1 && claude mcp remove claudy -s user >/dev/null 2>&1
pkill -x ClaudyManager 2>/dev/null
rm -f "$HOME/.local/bin/claudy" "$HOME/.local/share/applications/claudy.desktop" "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/claudy-manager.desktop"
rm -rf "$HOME/.local/share/claudy"
echo "Claudy 를 제거했습니다. 설정 폴더 ~/.claudy 는 남겨 두었습니다."
