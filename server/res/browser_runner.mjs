// Claudy browser_run 실행기. 사용: node runner.mjs <chrome|edge|aside> <작업 파일.mjs>
// 조작용 프로필(~/.claudy/browser/profiles/<이름>)과 원격 디버깅 포트로 브라우저를 띄우고(이미 떠 있으면 그대로)
// playwright-core 의 connectOverCDP 로 붙어 작업 파일의 기본 함수에 { page, context, browser } 를 넘긴다.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const BASE = path.join(os.homedir(), '.claudy', 'browser');
const require = createRequire(path.join(BASE, 'runner.cjs'));
const { chromium } = require('playwright-core');

const PORTS = { chrome: 9231, edge: 9232, aside: 9233 };

function candidates(name) {
  const pf = process.env['ProgramFiles'] || 'C:\\Program Files';
  const pf86 = process.env['ProgramFiles(x86)'] || 'C:\\Program Files (x86)';
  const la = process.env['LOCALAPPDATA'] || '';
  const home = os.homedir();
  if (process.platform === 'win32') {
    return {
      chrome: [`${pf}\\Google\\Chrome\\Application\\chrome.exe`, `${pf86}\\Google\\Chrome\\Application\\chrome.exe`, `${la}\\Google\\Chrome\\Application\\chrome.exe`],
      edge: [`${pf86}\\Microsoft\\Edge\\Application\\msedge.exe`, `${pf}\\Microsoft\\Edge\\Application\\msedge.exe`],
      aside: [`${pf}\\Aside\\Application\\Aside.exe`, `${la}\\Aside\\Application\\Aside.exe`],
    }[name];
  }
  if (process.platform === 'darwin') {
    return {
      chrome: ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', `${home}/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`],
      edge: ['/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge', `${home}/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge`],
      aside: ['/Applications/Aside.app/Contents/MacOS/Aside', `${home}/Applications/Aside.app/Contents/MacOS/Aside`],
    }[name];
  }
  return {
    chrome: ['/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chromium', '/usr/bin/chromium-browser', '/snap/bin/chromium'],
    edge: ['/usr/bin/microsoft-edge', '/usr/bin/microsoft-edge-stable'],
    aside: ['/usr/bin/aside'],
  }[name];
}

async function alive(port) {
  try {
    const r = await fetch(`http://127.0.0.1:${port}/json/version`, { signal: AbortSignal.timeout(1500) });
    return r.ok;
  } catch {
    return false;
  }
}

async function main() {
  const [, , name, jobFile] = process.argv;
  const port = PORTS[name];
  if (!port) throw new Error(`브라우저 이름은 chrome, edge, aside 중 하나다: ${name}`);
  if (!(await alive(port))) {
    const exe = (candidates(name) || []).find((p) => fs.existsSync(p));
    if (!exe) throw new Error(`${name} 실행 파일을 찾지 못했다`);
    const prof = path.join(BASE, 'profiles', name);
    fs.mkdirSync(prof, { recursive: true });
    const args = [`--remote-debugging-port=${port}`, `--user-data-dir=${prof}`, '--no-first-run', '--no-default-browser-check'];
    if (process.platform === 'win32') {
      // Windows 는 셸 실행(Start-Process)으로 띄운다. 직접 띄우면 브라우저가 호출한 쪽의 출력 파일 핸들을 물려받아 잡고 있는다
      const q = (x) => `'${x.replace(/'/g, "''")}'`;
      const cmd = `Start-Process -FilePath ${q(exe)} -ArgumentList ${args.map((a) => q(a.includes(' ') ? `"${a}"` : a)).join(',')}`;
      await new Promise((res) => spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', cmd], { stdio: 'ignore', windowsHide: true }).on('exit', res));
    } else {
      const child = spawn(exe, args, { detached: true, stdio: 'ignore' });
      child.unref();
    }
    const t0 = Date.now();
    while (!(await alive(port))) {
      if (Date.now() - t0 > 20000) throw new Error(`${name} 이 20초 안에 원격 디버깅 포트(${port})를 열지 않았다`);
      await new Promise((r) => setTimeout(r, 300));
    }
    console.error(`[claudy] ${name} 을 조작용 프로필로 띄웠다: ${prof}`);
  }
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  try {
    const context = browser.contexts()[0] || (await browser.newContext());
    const usable = () => context.pages().find((p) => !p.url().startsWith('chrome-extension://'));
    let page = usable();
    if (!page && process.env.CLAUDY_BG_WINDOW === '1') {
      // 가상 데스크톱 격리: 브라우저는 창 없이(--no-startup-window) 띄웠다. 창을 최소화·백그라운드로 만들어 전경을 가져가지 않게 하고,
      // 감시기가 그 창을 「Claudy 시험」 으로 옮겨 펼친다(11.25 실측: 사용자 전경 변화 없음)
      try {
        const s = await browser.newBrowserCDPSession();
        await s.send('Target.createTarget', { url: 'about:blank', newWindow: true, background: true, windowState: 'minimized' });
        for (let i = 0; i < 30 && !(page = usable()); i++) await new Promise((r) => setTimeout(r, 100));
      } catch {}
    }
    if (!page) page = await context.newPage();
    const job = await import(pathToFileURL(jobFile).href);
    const out = await job.default({ page, context, browser });
    if (out !== undefined) console.log(typeof out === 'string' ? out : JSON.stringify(out, null, 2));
  } finally {
    // browser.close() 는 브라우저 자체를 닫으므로 부르지 않는다. 프로세스를 끝내면 연결만 끊긴다
    try { fs.unlinkSync(jobFile); } catch {}
  }
}

main().then(() => process.exit(0)).catch((e) => {
  console.error(String(e && e.stack ? e.stack : e));
  process.exit(1);
});
