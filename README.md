# Claudy

Claudy 는 PuTTY 같은 SSH 접속 프로그램의 역할을 Claude 채팅에서 하도록 만든 로컬 MCP 서버입니다.
터미널에 직접 입력하던 명령을 채팅에서 지시하면, 이 PC 와 등록한 SSH 서버에서 실행하고 결과를 채팅으로 돌려줍니다.
각 호스트의 Claude Code 에도 작업을 맡깁니다.

- Windows·macOS 는 Claude 데스크톱 앱에 확장(.mcpb)으로 설치합니다.
- Linux 는 Claude Code 에 연결합니다(`claude mcp add`).
- 설정 앱(Claudy)으로 호스트 등록, 실행 기록 검색, 명령 차단 목록, 동작 설정을 관리합니다.

## 구성

| 폴더 | 내용 |
|---|---|
| `server/` | MCP 서버(Rust, rmcp). 실행 파일 하나 |
| `manager/` | 설정 앱(Rust, tao·wry). Windows WebView2, macOS WKWebView, Linux WebKitGTK |

## 도구 (서버 0.1.26, 13종)

| 도구 | 기능 |
|---|---|
| `hosts_list` | 등록된 호스트 목록 |
| `run_command` | 셸 명령 실행(출력, 종료 코드, 소요 시간) |
| `claude_ask` | 호스트의 Claude Code 에 작업 지시, 대화 이어가기 |
| `job_start` · `job_status` · `job_list` | 백그라운드 작업 |
| `file_copy` | 호스트 사이 파일·폴더 복사 |
| `app_open` | 호스트의 사용자 화면에 프로그램 실행 |
| `screen_capture` | 호스트의 사용자 화면 찍기 |
| `host_status` | 호스트의 Claudy 판, 터널·갱신 기록 |
| `claudy_update` | Windows 호스트의 Claudy 원격 갱신 |
| `ego_run` | ego 브라우저 스크립트 실행 |
| `browser_run` | Chrome·Edge·Aside 를 조작용 프로필로 띄우고 Playwright(CDP)로 조작 |

## 설정 파일

사용자 폴더의 `.claudy/hosts.toml` 에 둡니다. 설정 앱에서 고치거나 직접 편집합니다.

```toml
[[host]]
name = "this-pc"
kind = "local"

[[host]]
name = "build"
kind = "ssh"
host = "server.example.com"
port = 22
user = "user"
identity_file = "~/.ssh/id_ed25519_claudy"
# alt_host = "203.0.113.10"   # host 로 연결되지 않을 때 쓸 다른 주소
# jump = "relay"              # 먼저 거쳐 갈 ssh 호스트 이름
# shell = "powershell"        # 원격이 Windows 일 때
# desktop = true              # Windows 원격 명령을 로그온한 사용자 화면에서 실행
```

## 빌드

```sh
# 서버
cd server && cargo build --release
# 설정 앱 (Linux 는 libwebkit2gtk-4.1-dev, libayatana-appindicator3-dev 필요)
cd manager && cargo build --release
```

Windows 판은 `x86_64-pc-windows-gnu`, macOS 판은 `aarch64-apple-darwin`, Linux 판은 `x86_64-unknown-linux-gnu` 대상으로 빌드합니다.
설치 꾸러미(.mcpb)는 `server/mcpb/manifest.json` 과 실행 파일로 `npx @anthropic-ai/mcpb pack` 을 실행해 만듭니다.

## 보안

- 키 인증 SSH 만 씁니다. 설정 앱의 「비밀번호로 키 등록」은 비밀번호를 저장하지 않습니다.
- 정규식 차단 목록에 걸리는 명령은 실행하지 않습니다.
- 실행 기록에서 토큰·키 형태 문자열은 가려서 저장합니다.

## 라이선스

MIT. 설정 앱의 Pretendard 글꼴은 SIL Open Font License 1.1 을 따릅니다(`manager/LICENSE-Pretendard.txt`).
