# 개발 절차

이 저장소가 Claudy 소스의 원본이다. 고치고, 시험하고, 판을 올리는 일은 모두 이 저장소에서 한다.

## 판올림

1. `server/Cargo.toml` 의 `version` 과 `server/mcpb/manifest.json`·`manifest.mac.json` 의 `version` 을 함께 올린다
2. 설정 앱을 고쳤으면 `manager/Cargo.toml` 의 `version` 과 `manager/ui/index.html` 의 시연용 판 번호를 올린다
3. `main` 에 push 하면 「시험」 작업이 서버·설정 앱 시험을 돌린다
4. `git tag v<판> && git push origin v<판>` 을 하면 「릴리스」 작업이 Windows·macOS·Linux 배포 파일 4종을 만들어 GitHub 릴리스에 올린다

## 직접 빌드

| 위치 | 명령 | 결과 |
|---|---|---|
| Linux | `scripts/build_cross.sh` | `dist/bin/linux-x86_64`, `linux-arm64`, `windows` |
| macOS | `scripts/build_mac.sh` | `dist/bin/mac` (서명 스크립트를 `CLAUDY_SIGN` 으로 주면 그것으로 서명) |
| 어디서나 | `scripts/package.sh` | `dist/release` 의 zip·tar.gz |

macOS 판은 ad-hoc 서명이다. 받은 사람의 Mac 에서 처음 열 때 「확인되지 않은 개발자」 경고가 나오면 Finder 에서 오른쪽 클릭 → 열기로 연다.
