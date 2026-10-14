# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 JustMyth / Tan Ming Jing
#
# MSI smoke test on the CI runner: quiet per-machine install, check the files, the Start-menu
# and desktop shortcuts and the uninstall entry, launch the installed browser (it must open a
# window and stay up), then quiet uninstall and check it is gone. Logs go to -OutDir.
#
# v0.4.1 hardening (a private CI run: the hosted runner was lost 17 s into this step, no log
# survived). Every step is logged with a timestamp, so a runner loss shows where it happened.
# msiexec runs with a hard timeout and never asks Restart Manager to close other programs
# (MSIRESTARTMANAGERCONTROL=Disable, MSIDISABLERMRESTART=1, REBOOT=ReallySuppress): on a CI
# runner the "other programs" include the runner itself. The launched app gets
# MYTHBROWSE_NO_UPDATE_CHECK=1 (no GitHub API call from CI) and only processes started from the
# install folder are ever stopped. The workflow runs this in its own job (msi-smoke).
param([Parameter(Mandatory = $true)][string] $Msi, [Parameter(Mandatory = $true)][string] $OutDir)
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$status = 0
$t0 = Get-Date
function Log([string] $m) {
  $line = "[msi +{0,6:0.0}s] {1}" -f ((Get-Date) - $t0).TotalSeconds, $m
  Write-Host $line
  Add-Content -Path (Join-Path $OutDir "smoke.log") -Value $line
}
function Fail([string] $m) { Log "FAIL: $m"; $script:status = 1 }
function Snapshot([string] $when) {
  $os = Get-CimInstance Win32_OperatingSystem
  Log ("{0}: free memory {1:0} MB of {2:0} MB" -f $when, ($os.FreePhysicalMemory / 1KB), ($os.TotalVisibleMemorySize / 1KB))
  Get-Process | Sort-Object WorkingSet64 -Descending | Select-Object -First 8 |
    ForEach-Object { Log ("  {0,-28} pid {1,-6} {2,7:0} MB" -f $_.ProcessName, $_.Id, ($_.WorkingSet64 / 1MB)) }
}
# Windows Installer with a hard timeout; on timeout only that msiexec client is killed.
function Run-Msiexec([string[]] $msiArgs, [int] $seconds) {
  Log "msiexec $($msiArgs -join ' ')"
  $p = Start-Process msiexec.exe -ArgumentList $msiArgs -PassThru
  if (-not $p.WaitForExit($seconds * 1000)) {
    Log "msiexec (pid $($p.Id)) still running after $seconds s; stopping it"
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    return -1
  }
  return $p.ExitCode
}
# Only processes started from the install folder (never anything else on the runner).
function Stop-Installed([string] $dir) {
  Get-Process -Name mythbrowse -ErrorAction SilentlyContinue | Where-Object {
    try { $_.Path -and $_.Path.StartsWith($dir, [StringComparison]::OrdinalIgnoreCase) } catch { $false }
  } | ForEach-Object { Log "stopping installed mythbrowse pid $($_.Id)"; Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
}
$quiet = @("/qn", "/norestart", "MSIRESTARTMANAGERCONTROL=Disable", "MSIDISABLERMRESTART=1", "REBOOT=ReallySuppress")

Log "MSI: $Msi ($((Get-Item $Msi).Length) bytes)"
Snapshot "before install"
# Open-build identity (packaging/windows/mythbrowse.wxs defaults): side by side with official builds.
$product = "MythBrowse (Open Build)"
$dir = Join-Path $env:ProgramFiles "JustMyth\MythBrowse Open Build"
$code = Run-Msiexec (@("/i", "`"$Msi`"") + $quiet + @("/l*v", "`"$OutDir\install.log`"")) 300
Log "install exit code $code"
if ($code -ne 0 -and $code -ne 3010) { Fail "msiexec /i returned $code" }
foreach ($f in "mythbrowse.exe", "libEGL.dll", "libGLESv2.dll", "LICENSE", "NOTICE", "THIRD_PARTY_LICENSES.md", "THIRD_PARTY_NOTICES.txt") {
  if (-not (Test-Path (Join-Path $dir $f))) { Fail "$f not installed in $dir" }
}
Get-ChildItem $dir -ErrorAction SilentlyContinue | Format-Table Name, Length | Out-String | Write-Host
$lnk = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\$product.lnk"
if (Test-Path $lnk) { Log "Start-menu shortcut: $lnk" } else { Fail "no Start-menu shortcut at $lnk" }
# Desktop shortcut: on by default (the setup UI's options page), also for quiet installs.
$desk = Join-Path $env:PUBLIC "Desktop\$product.lnk"
if (Test-Path $desk) { Log "desktop shortcut: $desk" } else { Fail "no desktop shortcut at $desk" }
$arp = Get-ChildItem "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall" -ErrorAction SilentlyContinue |
  Get-ItemProperty | Where-Object { $_.DisplayName -eq $product }
if ($arp) { Log "uninstall entry: $($arp.DisplayName) $($arp.DisplayVersion) ($($arp.Publisher))" } else { Fail "no uninstall entry" }

# Launch the installed exe with its own profile; it must show a window and keep running.
$exe = Join-Path $dir "mythbrowse.exe"
if (Test-Path $exe) {
  $env:MYTHBROWSE_PROFILE_DIR = Join-Path $OutDir "profile"
  $env:MYTHBROWSE_NO_DIALOG = "1"
  $env:MYTHBROWSE_NO_UPDATE_CHECK = "1"
  Log "launching $exe"
  $app = Start-Process -FilePath $exe -WorkingDirectory $dir -PassThru `
    -RedirectStandardOutput (Join-Path $OutDir "launch-stdout.log") -RedirectStandardError (Join-Path $OutDir "launch-stderr.log")
  $null = $app.Handle
  $win = $false
  for ($i = 0; $i -lt 40; $i++) {
    Start-Sleep -Milliseconds 500
    $app.Refresh()
    if ($app.HasExited) { break }
    if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $win = $true; break }
  }
  Log "window: $win"
  Start-Sleep -Seconds 5
  $app.Refresh()
  if ($app.HasExited) { Fail "installed mythbrowse.exe exited early (code $($app.ExitCode))" }
  elseif (-not $win) { Fail "installed mythbrowse.exe showed no window" }
  else { Log "installed mythbrowse.exe is running with a window" }
  Snapshot "app running"
  if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue; Start-Sleep -Seconds 2 }
  Stop-Installed $dir
  Remove-Item Env:MYTHBROWSE_PROFILE_DIR, Env:MYTHBROWSE_NO_DIALOG, Env:MYTHBROWSE_NO_UPDATE_CHECK -ErrorAction SilentlyContinue
}

$code = Run-Msiexec (@("/x", "`"$Msi`"") + $quiet + @("/l*v", "`"$OutDir\uninstall.log`"")) 300
Log "uninstall exit code $code"
if ($code -ne 0 -and $code -ne 3010) { Fail "msiexec /x returned $code" }
if (Test-Path (Join-Path $dir "mythbrowse.exe")) { Fail "mythbrowse.exe still present after uninstall" }
if (Test-Path $lnk) { Fail "Start-menu shortcut still present after uninstall" }
if (Test-Path $desk) { Fail "desktop shortcut still present after uninstall" }
# A quiet install never launches the browser (the Finish page's checkbox needs the UI).
if (Get-Process -Name mythbrowse -ErrorAction SilentlyContinue) { Log "note: a mythbrowse process is still running" }
if ($status -eq 0) { Log "smoke test passed" }
exit $status
