# Claudy Windows 갱신기. claudy_update 도구가 Claudy\_upd 에 새 파일을 둔 뒤 이 스크립트를 따로 띄운다.
# 순서: 서버 교체(실행 중이어도 이름 바꾸기로 교체) → 설정 앱 교체 → 터널 확인 → 실패하면 이전 설정 앱으로 되돌리고, 그래도 안 되면 비상 터널.
# 결과는 Claudy\update.log 마지막 줄 RESULT ... 로 남긴다.
$ErrorActionPreference = 'Continue'
$d = Join-Path $env:USERPROFILE 'Claudy'
$u = Join-Path $d '_upd'
$log = Join-Path $d 'update.log'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$old = Join-Path $d "_old\$stamp"
function L($m) { Add-Content -Path $log -Value ("{0:yyyy-MM-dd HH:mm:ss} {1}" -f (Get-Date), $m) -Encoding UTF8 }
function StartMgr($exe) {
  # 로그온한 사용자 화면에서 실행한다(작업 스케줄러 COM, 경로에 공백이 있어도 된다)
  $svc = New-Object -ComObject Schedule.Service; $svc.Connect(); $f = $svc.GetFolder('\')
  $td = $svc.NewTask(0); $td.Principal.LogonType = 3
  $td.Settings.ExecutionTimeLimit = 'PT0S'; $td.Settings.DisallowStartIfOnBatteries = $false; $td.Settings.StopIfGoingOnBatteries = $false
  $a = $td.Actions.Create(0); $a.Path = $exe; $a.Arguments = '--background'
  $f.RegisterTaskDefinition('ClaudyManagerStart', $td, 6, $null, $null, 3) | Out-Null
  $f.GetTask('\ClaudyManagerStart').Run($null) | Out-Null
  Start-Sleep 3
  $f.DeleteTask('ClaudyManagerStart', 0)
}
function TunnelOk($since) {
  $tl = Join-Path $env:USERPROFILE '.claudy\tunnel.log'
  for ($i = 0; $i -lt 12; $i++) {
    Start-Sleep 5
    $line = Get-Content $tl -Encoding UTF8 -Tail 1 -ErrorAction SilentlyContinue
    if ($line -match '^(\S+ \S+) ssh .*pid (\d+)$') {
      $when = [datetime]::ParseExact($Matches[1], 'yyyy-MM-dd HH:mm:ss', $null)
      if ($when -ge $since.AddSeconds(-2) -and ((Get-Date) - $when).TotalSeconds -ge 10 -and (Get-Process -Id ([int]$Matches[2]) -ErrorAction SilentlyContinue)) { return $true }
    }
  }
  return $false
}
function TunnelOn() {
  $m = Join-Path $env:USERPROFILE '.claudy\manager.toml'
  return (Test-Path $m) -and ((Get-Content $m -Raw -Encoding UTF8) -match '(?m)^tunnel_enabled\s*=\s*true')
}
L "갱신 시작"
# 이 갱신기를 띄운 접속이 결과를 받고 닫힐 시간을 준다(설정 앱을 끄면 터널이 잠시 끊긴다)
Start-Sleep 8
New-Item -ItemType Directory -Force $old | Out-Null
$result = 'ok'
$srv = Join-Path $u 'claudy.exe'
if (Test-Path $srv) {
  $cur = Join-Path $d 'server\claudy.exe'
  if (Test-Path $cur) { Move-Item -Force $cur (Join-Path $old 'claudy.exe') }
  Copy-Item -Force $srv $cur
  L "서버 교체(Claude 앱을 다시 켜면 적용)"
}
$man = Join-Path $u 'manifest.json'
if (Test-Path $man) {
  $curm = Join-Path $d 'manifest.json'
  if (Test-Path $curm) { Copy-Item -Force $curm (Join-Path $old 'manifest.json') }
  Copy-Item -Force $man $curm
  L "manifest 교체"
}
$mgr = Join-Path $u 'ClaudyManager.exe'
if (Test-Path $mgr) {
  $curg = Join-Path $d 'ClaudyManager.exe'
  Get-Process ClaudyManager -ErrorAction SilentlyContinue | Stop-Process -Force
  Start-Sleep 2
  if (Test-Path $curg) { Copy-Item -Force $curg (Join-Path $old 'ClaudyManager.exe') }
  Copy-Item -Force $mgr $curg
  $t0 = Get-Date
  StartMgr $curg
  L "설정 앱 교체 후 실행"
  if (TunnelOn) {
    if (TunnelOk $t0) { L "터널 확인됨" } else {
      L "터널 확인 실패. 이전 설정 앱으로 되돌림"
      $result = 'rollback'
      Get-Process ClaudyManager -ErrorAction SilentlyContinue | Stop-Process -Force
      Start-Sleep 2
      $prev = Join-Path $old 'ClaudyManager.exe'
      if (Test-Path $prev) { Copy-Item -Force $prev $curg }
      $t1 = Get-Date
      StartMgr $curg
      if (TunnelOk $t1) { L "이전 설정 앱으로 터널 복구" } else {
        $tp = Join-Path $d 'tunnel\tunnel.ps1'
        if (Test-Path $tp) { Start-Process powershell.exe -ArgumentList "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$tp`"" -WindowStyle Hidden; L "비상 터널 실행" }
        $result = 'fail'
      }
    }
  }
}
Remove-Item -Recurse -Force $u -ErrorAction SilentlyContinue
L "RESULT $result"
