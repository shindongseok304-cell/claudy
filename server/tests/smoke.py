#!/usr/bin/env python3
"""Claudy 통합 시험: stdio JSON-RPC 로 실제 바이너리를 구동한다.

- 임시 HOME 을 만들고 hosts.toml 에 this-pc(sh) 와 가짜 claude 스크립트를 등록한다.
- 가짜 claude 는 stdin 을 읽어 result JSON 한 줄을 출력한다.
- 실제 원격 서버에는 접속하지 않는다.

사용법: python3 tests/smoke.py [바이너리 경로]
"""
import json
import os
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "release", "claudy")
SCRATCH = os.environ.get("SMOKE_TMP") or None

FAKE_CLAUDE = """#!/usr/bin/env python3
import json, sys
prompt = sys.stdin.read()
print(json.dumps({"type": "result", "subtype": "success", "is_error": False,
                  "result": prompt, "session_id": "abc",
                  "total_cost_usd": 0.01, "num_turns": 1}))
"""

FAKE_ARGV = """#!/usr/bin/env python3
import json, sys
sys.stdin.read()
print(json.dumps({"type": "result", "subtype": "success", "is_error": False,
                  "result": json.dumps(sys.argv[1:]), "session_id": "argv",
                  "total_cost_usd": 0.0, "num_turns": 1}))
"""

failures = []


def check(cond, label):
    print(("PASS " if cond else "FAIL ") + label)
    if not cond:
        failures.append(label)


class Client:
    def __init__(self, home):
        env = dict(os.environ, HOME=home, USERPROFILE=home)
        self.p = subprocess.Popen([BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, env=env)
        self.next_id = 1

    def send(self, method, params=None, notify=False):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        if not notify:
            msg["id"] = self.next_id
            self.next_id += 1
        self.p.stdin.write((json.dumps(msg) + "\n").encode())
        self.p.stdin.flush()
        if notify:
            return None
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("서버 종료됨: " + self.p.stderr.read().decode(errors="replace"))
            data = json.loads(line)
            if data.get("id") == msg["id"]:
                return data

    def call(self, name, args):
        r = self.send("tools/call", {"name": name, "arguments": args})
        res = r["result"]
        text = "".join(c.get("text", "") for c in res.get("content", []))
        return res.get("isError", False), text

    def close(self):
        self.p.stdin.close()
        try:
            self.p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.p.kill()


def main():
    home = tempfile.mkdtemp(prefix="claudy-home-", dir=SCRATCH)
    fake = os.path.join(home, "fake_claude.py")
    with open(fake, "w") as f:
        f.write(FAKE_CLAUDE)
    os.chmod(fake, 0o755)
    argv_fake = os.path.join(home, "argv_claude.py")
    with open(argv_fake, "w") as f:
        f.write(FAKE_ARGV)
    os.chmod(argv_fake, 0o755)
    cfg_dir = os.path.join(home, ".claudy")
    os.makedirs(cfg_dir, exist_ok=True)
    with open(os.path.join(cfg_dir, "hosts.toml"), "w") as f:
        f.write('[[host]]\nname = "this-pc"\nkind = "local"\nshell = "sh"\n'
                'claude = "%s"\nworkdir = ""\n\n' % fake)
        f.write('[[host]]\nname = "argv-default"\nkind = "local"\nshell = "sh"\n'
                'claude = "%s"\nclaude_permissions = "default"\n\n' % argv_fake)
        f.write('[[host]]\nname = "argv-accept"\nkind = "local"\nshell = "sh"\n'
                'claude = "%s"\nclaude_permissions = "accept_edits"\n' % argv_fake)

    c = Client(home)
    init = c.send("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "smoke", "version": "0"}})
    check("result" in init, "initialize 응답")
    c.send("notifications/initialized", notify=True)

    tools = c.send("tools/list")["result"]["tools"]
    names = sorted(t["name"] for t in tools)
    check(names == sorted(["hosts_list", "run_command", "claude_ask", "job_start", "job_status", "job_list"]),
          "tools/list 6개 확인: " + ",".join(names))

    err, text = c.call("hosts_list", {})
    check(not err and "this-pc" in text, "hosts_list 에 this-pc 표시")
    check(not err and '"claude_permissions": "default"' in text and '"claude_permissions": "skip"' in text,
          "hosts_list 에 claude_permissions 표시")

    props = {}
    for t in tools:
        props[t["name"]] = list(t.get("inputSchema", {}).get("properties", {}).keys())
    check(all("session_id" not in v for v in props.values()) and "resume_id" in props["claude_ask"],
          "인자 이름에 session_id 없음 (resume_id 사용)")

    err, text = c.call("run_command", {"host": "this-pc", "command": "sleep 5", "timeout_sec": 1})
    check(err and "제한 시간" in text, "run_command 시간 초과 중단")

    err, text = c.call("claude_ask", {"host": "argv-default", "prompt": "p"})
    check(not err and "--dangerously-skip-permissions" not in text and "--permission-mode" not in text
          and "--output-format" in text,
          "claude_permissions=default 에는 권한 인자 없음")
    err, text = c.call("claude_ask", {"host": "argv-accept", "prompt": "p"})
    check(not err and "--permission-mode" in text and "acceptEdits" in text
          and "--dangerously-skip-permissions" not in text, "claude_permissions=accept_edits 인자")
    err, text = c.call("claude_ask", {"host": "this-pc", "prompt": "p"})
    check(not err and '"result": "p"' in text, "기본 skip 호스트는 기존 동작")

    err, text = c.call("run_command", {"host": "this-pc", "command": "echo hello"})
    check(not err and "hello" in text and "exit_code: 0" in text, "run_command echo hello")

    err, text = c.call("run_command", {"host": "this-pc", "command": "echo password=hunter2; echo sk-ant-ABC123"})
    check(not err and "hunter2" not in text and "sk-ant-ABC123" not in text and "***" in text,
          "run_command 출력의 비밀 값 가림")

    err, text = c.call("run_command", {"host": "this-pc", "command": "rm -rf /"})
    check(err and "차단" in text, "차단 명령 rm -rf / 거부")

    err, text = c.call("run_command", {"host": "nope", "command": "echo x"})
    check(err and "알 수 없는 host" in text, "알 수 없는 host 오류")

    err, text = c.call("claude_ask", {"host": "this-pc", "prompt": "안녕 Claude"})
    check(not err and '"result": "안녕 Claude"' in text and '"session_id": "abc"' in text
          and '"total_cost_usd": 0.01' in text, "claude_ask 결과 JSON 해석")

    err, text = c.call("claude_ask", {"host": "this-pc", "prompt": "x", "session_id": "bad;id"})
    check(err and "session_id" in text, "claude_ask 잘못된 session_id 거부")

    err, text = c.call("job_start", {"host": "this-pc", "kind": "command", "command": "sleep 2; echo done"})
    check(not err, "job_start command 시작")
    text_start = text
    job_id = None
    try:
        job_id = json.loads(text)["job_id"]
    except Exception:
        pass
    finished = False
    last = ""
    for _ in range(40):
        err, last = c.call("job_status", {"job_id": job_id or "x"})
        if '"finished": true' in last:
            finished = True
            break
        time.sleep(0.5)
    check(finished and "done" in last and '"exit_code": 0' in last, "job_status 로 백그라운드 완료 확인")
    status_file = json.loads(text_start)["status_file"] if job_id else None
    with open(status_file, encoding="utf-8") as f:
        st = json.load(f)
    check(isinstance(st.get("pid"), int) and st.get("pid") > 0, "작업 상태 파일에 pid 기록")

    dead = os.path.join(cfg_dir, "jobs", "deadjob01.json")
    with open(dead, "w", encoding="utf-8") as f:
        json.dump({"job_id": "deadjob01", "host": "this-pc", "kind": "command", "started_at": "2026-01-01T00:00:00+09:00",
                   "finished": False, "exit_code": None, "ended_at": None, "pid": 999999}, f)
    err, last = c.call("job_status", {"job_id": "deadjob01"})
    check(not err and '"interrupted": true' in last and '"finished": true' in last, "죽은 pid 작업은 interrupted 로 보임")
    with open(dead, encoding="utf-8") as f:
        st2 = json.load(f)
    check(st2.get("interrupted") is True and st2.get("finished") is True, "죽은 pid 작업의 상태 파일도 갱신")
    err, lst = c.call("job_list", {})
    check(not err and '"interrupted": true' in lst, "job_list 에도 interrupted 표시")

    err, text = c.call("job_start", {"host": "this-pc", "kind": "claude", "prompt": "백그라운드 질문"})
    jid = json.loads(text)["job_id"] if not err else None
    done = False
    last = ""
    for _ in range(40):
        time.sleep(0.5)
        err, last = c.call("job_status", {"job_id": jid or "x"})
        if '"finished": true' in last:
            done = True
            break
    check(done and '"result": "백그라운드 질문"' in last, "job_start claude 결과 해석")

    err, text = c.call("job_list", {})
    check(not err and job_id in text, "job_list 에 작업 표시")

    audit_dir = os.path.join(cfg_dir, "logs")
    logs = os.listdir(audit_dir) if os.path.isdir(audit_dir) else []
    audit_text = "".join(open(os.path.join(audit_dir, n), encoding="utf-8").read() for n in logs)
    check(bool(logs) and '"denied":true' in audit_text.replace(" ", ""), "감사 로그 기록(거부 포함)")
    check("hunter2" not in audit_text, "감사 로그에 비밀 값 없음")

    c.close()
    print("\n%d 항목 실패" % len(failures))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
