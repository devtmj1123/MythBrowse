# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 JustMyth / Tan Ming Jing
#
# Smoke test for the Windows release zip on a GPU-less CI runner, run exactly as a user
# would: the unpacked zip, double-click equivalent (no console, no extra files). Servo's
# surfman renders through ANGLE (libEGL/libGLESv2 -> Direct3D 11), which on the runner
# falls back to Microsoft's WARP software rasterizer, so no GPU is needed.
#  0. negative check: mythbrowse.exe copied alone must exit with code 1 and a clear
#     "libEGL.dll missing" log line (not a panic);
#  1. AI home, then address-bar navigation (Ctrl+L, "example.com", Enter);
#  2. https://example.com from the command line;
#  3. the frameless window itself: a normal (not maximized) window, then the custom title
#     bar's maximize and restore buttons, a title-bar drag, and the close button (must exit
#     cleanly);
#  4. pointer mapping into the floating page card (a click on a tiny link must navigate) and
#     a mouse click into the address bar followed by typing.
#  4b. WebCrypto (ci/webcrypto-check.html on a loopback server): ML-DSA-44/65/87 reject with
#     NotSupportedError, SHA-256 / HMAC / AES-GCM / ECDSA / Ed25519 / RSA-PSS / RSASSA /
#     RSA-OAEP / PBKDF2 / HKDF still work (known-answer vectors where standard);
#  5. Myth against a mock OpenAI-compatible server: streamed chat from the home page and a
#     question typed on a web page (the open build answers, never acts; no tools are sent);
#     DPAPI key store self-test; no API key in any log.
# Each run pins the time-of-day theme (MYTHBROWSE_THEME_HOUR) so light and dark are covered.
# Takes full-screen screenshots; fails if the browser exits early / panics.
# (-WithMesa additionally drops Mesa3D's opengl32.dll next to the exe; only relevant if the
# WGL path is ever used again.)
#
# Usage: ci/smoke-windows.ps1 -AppDir <unpacked zip> -OutDir <output dir> [-WithMesa]
param(
  [Parameter(Mandatory = $true)] [string] $AppDir,
  [Parameter(Mandatory = $true)] [string] $OutDir,
  [switch] $WithMesa
)
$ErrorActionPreference = "Continue"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$AppDir = (Resolve-Path $AppDir).Path
$OutDir = (Resolve-Path $OutDir).Path
$exe = Join-Path $AppDir "mythbrowse.exe"
$status = 0

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  // Client area in screen coordinates: left, top, width, height.
  public static int[] Client(IntPtr h) {
    RECT r; GetClientRect(h, out r); POINT p = new POINT(); ClientToScreen(h, ref p);
    return new int[] { p.X, p.Y, r.Right - r.Left, r.Bottom - r.Top };
  }
  public static void Down() { mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero); }
  public static void Up() { mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero); }
}
"@
[W]::SetProcessDPIAware() | Out-Null

function Shot([string] $name) {
  try {
    $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
    $bmp.Save((Join-Path $OutDir "$name.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Host "[smoke] screenshot $name.png ($($b.Width)x$($b.Height))"
  } catch { Write-Host "[smoke] screenshot $name failed: $_" }
}

if ($WithMesa) {
  Write-Host "[smoke] fetching Mesa3D for Windows (llvmpipe)"
  $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/pal1000/mesa-dist-win/releases/latest" -Headers @{ "User-Agent" = "mythbrowse-ci" }
  $asset = $rel.assets | Where-Object { $_.name -match "^mesa3d-.*-release-msvc\.7z$" } | Select-Object -First 1
  Write-Host "[smoke] Mesa: $($rel.tag_name) $($asset.name)"
  $archive = Join-Path $env:RUNNER_TEMP $asset.name
  Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $archive
  $mesaDir = Join-Path $env:RUNNER_TEMP "mesa"
  & 7z x -y "-o$mesaDir" $archive | Out-Null
  Get-ChildItem (Join-Path $mesaDir "x64") -Filter *.dll | ForEach-Object {
    Copy-Item $_.FullName $AppDir -Force
    Write-Host "[smoke]   + $($_.Name)"
  }
  $env:GALLIUM_DRIVER = "llvmpipe"
}
$env:RUST_LOG = "info"
$env:RUST_BACKTRACE = "1"
$env:MYTHBROWSE_NO_DIALOG = "1"          # a modal error box would hang the job
# Each run gets its own profile dir (history DB + logs\mythbrowse.log): profile-<run>.
Write-Host "[smoke] app dir:"; Get-ChildItem $AppDir | Format-Table Name, Length | Out-String | Write-Host

function Show-Logs([string] $tag) {
  foreach ($f in @("$tag-stdout.log", "$tag-stderr.log")) {
    $path = Join-Path $OutDir $f
    if (Test-Path $path) { Write-Host "----- $f"; Get-Content $path -Tail 40 | Write-Host }
  }
  $log = Join-Path $OutDir "profile-$tag\logs\mythbrowse.log"
  if (Test-Path $log) { Write-Host "----- profile log"; Get-Content $log -Tail 40 | Write-Host }
}

# Run 0: the exe alone (what happens when a user extracts only mythbrowse.exe).
$lone = Join-Path $env:RUNNER_TEMP "lone"
New-Item -ItemType Directory -Force -Path $lone | Out-Null
Copy-Item $exe $lone
$env:MYTHBROWSE_PROFILE_DIR = Join-Path $OutDir "profile-run0"
$p0 = Start-Process -FilePath (Join-Path $lone "mythbrowse.exe") -PassThru -WorkingDirectory $lone `
  -RedirectStandardOutput (Join-Path $OutDir "run0-stdout.log") -RedirectStandardError (Join-Path $OutDir "run0-stderr.log")
$null = $p0.Handle   # cache the handle so ExitCode is available later
if (-not $p0.WaitForExit(30000)) { Stop-Process -Id $p0.Id -Force; Write-Host "[smoke] run0 did not exit (expected a fast, clean failure)"; $status = 1 }
else {
  Write-Host "[smoke] run0 (exe alone) exit code: $($p0.ExitCode)"
  $err0 = Get-Content (Join-Path $OutDir "run0-stderr.log") -Raw
  if ($p0.ExitCode -ne 1 -or $err0 -match "panicked") { Write-Host "[smoke] run0: expected exit code 1 without a panic"; $status = 1 }
  if ($err0 -notmatch "libEGL.dll") { Write-Host "[smoke] run0: expected a libEGL.dll message"; $status = 1 }
}
Show-Logs "run0"

function Start-Browser([string] $tag, [string[]] $argv) {
  $p = @{ FilePath = $exe; PassThru = $true; WorkingDirectory = $AppDir
          RedirectStandardOutput = (Join-Path $OutDir "$tag-stdout.log")
          RedirectStandardError = (Join-Path $OutDir "$tag-stderr.log") }
  if ($argv) { $p.ArgumentList = $argv }
  $env:MYTHBROWSE_PROFILE_DIR = Join-Path $OutDir "profile-$tag"
  $proc = Start-Process @p
  $null = $proc.Handle   # cache the handle so ExitCode is available later
  for ($i = 0; $i -lt 60; $i++) {
    Start-Sleep -Milliseconds 500
    $proc.Refresh()
    if ($proc.HasExited -or $proc.MainWindowHandle -ne [IntPtr]::Zero) { break }
  }
  if ($proc.HasExited) { Write-Host "[smoke] $tag exited early, code $($proc.ExitCode)" }
  elseif ($proc.MainWindowHandle -eq [IntPtr]::Zero) { Write-Host "[smoke] $tag : no window yet" }
  else { Write-Host "[smoke] $tag window: '$($proc.MainWindowTitle)'" }
  return $proc
}

function Stop-Browser($proc, [string] $tag) {
  $proc.Refresh()
  if ($proc.HasExited) {
    Write-Host "[smoke] $tag EXITED EARLY with code $($proc.ExitCode)"
    $script:status = 1
    Show-Logs $tag
  } else {
    Write-Host "[smoke] $tag still alive (good), title '$($proc.MainWindowTitle)'"
    Stop-Process -Id $proc.Id -Force
  }
  $err = Get-Content (Join-Path $OutDir "$tag-stderr.log") -Raw -ErrorAction SilentlyContinue
  if ($err -match "panicked") { Write-Host "[smoke] $tag : PANIC in stderr"; $script:status = 1 }
  Start-Sleep -Seconds 2
}

function Client([IntPtr] $h) { $c = [W]::Client($h); return "x=$($c[0]) y=$($c[1]) w=$($c[2]) h=$($c[3])" }
function Click([int] $x, [int] $y) {
  [W]::SetCursorPos($x, $y) | Out-Null; Start-Sleep -Milliseconds 250
  [W]::Down(); Start-Sleep -Milliseconds 120; [W]::Up()
}

# Run 1: AI home (day theme), then address-bar navigation via keyboard.
$env:MYTHBROWSE_THEME_HOUR = "10"
$p1 = Start-Browser "run1" @()
if ($p1.MainWindowHandle -ne [IntPtr]::Zero) {
  [W]::ShowWindow($p1.MainWindowHandle, 3) | Out-Null   # maximise to fit the 1024x768 screen
}
Start-Sleep -Seconds 20
Shot "01-home"
$p1.Refresh()
if (-not $p1.HasExited -and $p1.MainWindowHandle -ne [IntPtr]::Zero) {
  [W]::SetForegroundWindow($p1.MainWindowHandle) | Out-Null
  Start-Sleep -Seconds 1
  [System.Windows.Forms.SendKeys]::SendWait("^l"); Start-Sleep -Seconds 1
  [System.Windows.Forms.SendKeys]::SendWait("example.com"); Start-Sleep -Milliseconds 500
  Shot "02-typed"
  [System.Windows.Forms.SendKeys]::SendWait("{ENTER}")
  Start-Sleep -Seconds 15
  Shot "03-after-enter"
  [System.Windows.Forms.SendKeys]::SendWait("^h"); Start-Sleep -Seconds 6
  Shot "07-library"
}
Stop-Browser $p1 "run1"

# Run 2: example.com from the command line (night theme).
$env:MYTHBROWSE_THEME_HOUR = "21.5"
$p2 = Start-Browser "run2" @("https://example.com")
if ($p2.MainWindowHandle -ne [IntPtr]::Zero) { [W]::ShowWindow($p2.MainWindowHandle, 3) | Out-Null }
Start-Sleep -Seconds 20
Shot "05-cli-example"
Stop-Browser $p2 "run2"

# Run 3: the frameless window (dusk theme): normal window, maximize / restore buttons,
# title-bar drag, close button.
$env:MYTHBROWSE_THEME_HOUR = "18.4"
$p3 = Start-Browser "run3" @()
$h3 = $p3.MainWindowHandle
if ($h3 -ne [IntPtr]::Zero) {
  [W]::MoveWindow($h3, 40, 24, 940, 660, $true) | Out-Null
  [W]::SetForegroundWindow($h3) | Out-Null
  Start-Sleep -Seconds 18
  Shot "06-window-dusk"
  $c = [W]::Client($h3)
  Write-Host "[smoke] run3 client before: $(Client $h3) zoomed=$([W]::IsZoomed($h3))"
  # Window controls are 46 px wide each at the right end of the 40 px title bar.
  Click ($c[0] + $c[2] - 69) ($c[1] + 20); Start-Sleep -Seconds 4
  $zoomed = [W]::IsZoomed($h3)
  Write-Host "[smoke] run3 after maximize click: zoomed=$zoomed $(Client $h3)"
  Shot "08-after-max-click"
  if (-not $zoomed) { Write-Host "[smoke] run3: maximize button did not maximize"; $status = 1 }
  $c = [W]::Client($h3)
  Click ($c[0] + $c[2] - 69) ($c[1] + 20); Start-Sleep -Seconds 4
  $zoomed = [W]::IsZoomed($h3)
  Write-Host "[smoke] run3 after restore click: zoomed=$zoomed $(Client $h3)"
  if ($zoomed) { Write-Host "[smoke] run3: restore button did not restore"; $status = 1 }
  # Drag the window by an empty part of the title bar.
  $c = [W]::Client($h3)
  $sx = $c[0] + $c[2] - 300; $sy = $c[1] + 20
  [W]::SetCursorPos($sx, $sy) | Out-Null; Start-Sleep -Milliseconds 300
  [W]::Down(); Start-Sleep -Milliseconds 400
  for ($i = 1; $i -le 10; $i++) { [W]::SetCursorPos($sx - 6 * $i, $sy + 4 * $i) | Out-Null; Start-Sleep -Milliseconds 60 }
  Start-Sleep -Milliseconds 300; [W]::Up(); Start-Sleep -Seconds 3
  $d = [W]::Client($h3)
  Write-Host "[smoke] run3 drag: moved by dx=$($d[0] - $c[0]) dy=$($d[1] - $c[1]) (expected about -60, +40)"
  Shot "09-after-drag"
  if ($d[0] -eq $c[0] -and $d[1] -eq $c[1]) { Write-Host "[smoke] run3: WARNING title-bar drag did not move the window (synthetic input)" }
  # Close button: the browser must exit cleanly.
  Click ($d[0] + $d[2] - 23) ($d[1] + 20)
  if ($p3.WaitForExit(15000)) {
    Write-Host "[smoke] run3: close button exited the browser, code $($p3.ExitCode)"
    if ($p3.ExitCode -ne 0) { $status = 1 }
  } else {
    Write-Host "[smoke] run3: close button did not exit the browser"; $status = 1
    Stop-Process -Id $p3.Id -Force
  }
} else {
  Write-Host "[smoke] run3: no window"; $status = 1
}
Show-Logs "run3"

# Run 4: pointer mapping into the floating page card (day theme, normal window). The page is a
# 12x12 link at (20,20) in page coordinates; the card starts at (8, 88+8) in the window at
# 100 % scale, so a click at client (34,122) only hits it if the shell offsets pointer events
# by the card's origin. Then a mouse click into the address bar, typing and Enter.
$env:MYTHBROWSE_THEME_HOUR = "10"
$clickUrl = "data:text/html,<title>click-test</title><body%20style=margin:0;background:white><a%20href=https://example.org/%20style=display:block;position:fixed;left:20px;top:20px;width:12px;height:12px;background:red></a>"
$p4 = Start-Browser "run4" @($clickUrl)
$h4 = $p4.MainWindowHandle
if ($h4 -ne [IntPtr]::Zero) {
  [W]::MoveWindow($h4, 40, 24, 940, 660, $true) | Out-Null
  [W]::SetForegroundWindow($h4) | Out-Null
  Start-Sleep -Seconds 15
  Shot "10-card-window-day"
  $p4.Refresh(); Write-Host "[smoke] run4 title before click: '$($p4.MainWindowTitle)'"
  $c = [W]::Client($h4)
  Click ($c[0] + 34) ($c[1] + 122); Start-Sleep -Seconds 15
  $p4.Refresh(); $t = $p4.MainWindowTitle
  Write-Host "[smoke] run4 title after page click: '$t'"
  Shot "11-after-page-click"
  if ($t -notmatch "Example Domain") { Write-Host "[smoke] run4: click on the page link did not navigate (pointer offset?)"; $status = 1 }
  # Address bar by mouse: centre of the toolbar row (title bar 40 px + toolbar 48 px).
  $c = [W]::Client($h4)
  Click ($c[0] + [int]($c[2] / 2)) ($c[1] + 64); Start-Sleep -Seconds 1
  [System.Windows.Forms.SendKeys]::SendWait("^a"); Start-Sleep -Milliseconds 300
  [System.Windows.Forms.SendKeys]::SendWait("data:text/html,<title>typed-ok</title>"); Start-Sleep -Milliseconds 500
  Shot "12-omnibox-click-typed"
  [System.Windows.Forms.SendKeys]::SendWait("{ENTER}"); Start-Sleep -Seconds 6
  $p4.Refresh(); $t = $p4.MainWindowTitle
  Write-Host "[smoke] run4 title after address-bar click + Enter: '$t'"
  if ($t -notmatch "typed-ok") { Write-Host "[smoke] run4: address bar click/typing did not navigate"; $status = 1 }
} else {
  Write-Host "[smoke] run4: no window"; $status = 1
}
Stop-Browser $p4 "run4"

# Run 4b: WebCrypto in a secure context (http://127.0.0.1, served by ci/webcrypto_check.py).
# The page sets its title to "webcrypto PASS n/n" or "webcrypto FAIL k/n" and POSTs a JSON report.
$wcLog = Join-Path $OutDir "webcrypto.json"
$wc = Start-Process -FilePath "python" -ArgumentList @("ci/webcrypto_check.py", "--port", "18081", "--log", $wcLog) `
  -PassThru -RedirectStandardOutput (Join-Path $OutDir "webcrypto-stdout.log") -RedirectStandardError (Join-Path $OutDir "webcrypto-stderr.log")
$wcUp = $false
for ($i = 0; $i -lt 60 -and -not $wcUp; $i++) {
  try { $tc = New-Object System.Net.Sockets.TcpClient; $tc.Connect("127.0.0.1", 18081); $tc.Close(); $wcUp = $true }
  catch { Start-Sleep -Milliseconds 500 }
}
if (-not $wcUp) { Write-Host "[smoke] run4b: WebCrypto page server did not start"; $status = 1 }
$env:MYTHBROWSE_THEME_HOUR = "10"
$p4b = Start-Browser "run4b" @("http://127.0.0.1:18081/")
$t = ""
for ($i = 0; $i -lt 120; $i++) {
  Start-Sleep -Seconds 1
  $p4b.Refresh()
  if ($p4b.HasExited) { break }
  $t = $p4b.MainWindowTitle
  if ($t -match "webcrypto (PASS|FAIL)") { break }
}
Write-Host "[smoke] run4b title: '$t' after ~$i s"
Shot "13-webcrypto"
Stop-Browser $p4b "run4b"
Stop-Process -Id $wc.Id -Force -ErrorAction SilentlyContinue
$wcr = Get-Content $wcLog -Raw -ErrorAction SilentlyContinue
Write-Host "----- webcrypto.json"; Write-Host $wcr
$wcOk = $false
try { $j = ($wcr.Trim() -split "`n")[-1] | ConvertFrom-Json; $wcOk = ($j.ok -eq $true -and $j.total -ge 22)
  foreach ($r in $j.results) { Write-Host ("[smoke] run4b " + ($(if ($r.ok) { "PASS " } else { "FAIL " })) + $r.name + " " + $r.detail) } } catch { }
if ($t -notmatch "webcrypto PASS" -or -not $wcOk) { Write-Host "[smoke] run4b: WebCrypto check FAILED"; $status = 1 }
else { Write-Host "[smoke] run4b: WebCrypto check passed ($($j.passed)/$($j.total))" }

# Run 5: Myth with a mock OpenAI-compatible model (ci/mock_chat.py): a streamed chat reply on
# the home page, then a question typed in the address bar on a web page. The open build never
# sends tool definitions. Also: the DPAPI key store self-test, and the API key must never
# appear in any log.
$key = "sk-smoke-" + [guid]::NewGuid().ToString("N")
$mockLog = Join-Path $OutDir "mock-chat.log"
$mock = Start-Process -FilePath "python" -ArgumentList @("ci/mock_chat.py", "--port", "18080", "--key", $key, "--log", $mockLog) `
  -PassThru -RedirectStandardOutput (Join-Path $OutDir "mock-stdout.log") -RedirectStandardError (Join-Path $OutDir "mock-stderr.log")
# A cold Python on the runner can take several seconds to bind: wait for the port, not a guess.
$mockUp = $false
for ($i = 0; $i -lt 60 -and -not $mockUp; $i++) {
  try { $tc = New-Object System.Net.Sockets.TcpClient; $tc.Connect("127.0.0.1", 18080); $tc.Close(); $mockUp = $true }
  catch { Start-Sleep -Milliseconds 500 }
}
if ($mockUp) { Write-Host "[smoke] mock model listening after ~$([int]($i / 2)) s" }
else { Write-Host "[smoke] run5: mock model did not start listening within 30 s"; $status = 1 }

$env:MYTHBROWSE_PROFILE_DIR = Join-Path $OutDir "profile-keystore"
$ks = Start-Process -FilePath $exe -ArgumentList @("--keystore-selftest") -Wait -PassThru -WorkingDirectory $AppDir `
  -RedirectStandardOutput (Join-Path $OutDir "keystore-stdout.log") -RedirectStandardError (Join-Path $OutDir "keystore-stderr.log")
Write-Host "[smoke] keystore self-test exit code: $($ks.ExitCode)"
Get-Content (Join-Path $OutDir "profile-keystore\logs\mythbrowse.log") -ErrorAction SilentlyContinue | Select-String "keystore" | Write-Host
if ($ks.ExitCode -ne 0) { Write-Host "[smoke] keystore self-test FAILED"; $status = 1 }

$prof5 = Join-Path $OutDir "profile-run5"
New-Item -ItemType Directory -Force -Path $prof5 | Out-Null
@"
default_provider = "main"

[providers.main]
kind = "openai_compatible"
base_url = "http://127.0.0.1:18080/v1"
model = "mock-myth-1"
api_key_env = "MYTHBROWSE_SMOKE_KEY"
"@ | Out-File -FilePath (Join-Path $prof5 "ai.toml") -Encoding ascii
$env:MYTHBROWSE_SMOKE_KEY = $key
$env:MYTHBROWSE_THEME_HOUR = "10"
$p5 = Start-Browser "run5" @("Say hello to the smoke test")
if ($p5.MainWindowHandle -ne [IntPtr]::Zero) {
  [W]::ShowWindow($p5.MainWindowHandle, 3) | Out-Null
  Start-Sleep -Seconds 14
  Shot "20-chat"
  [W]::SetForegroundWindow($p5.MainWindowHandle) | Out-Null
  Start-Sleep -Seconds 1
  [System.Windows.Forms.SendKeys]::SendWait("^t"); Start-Sleep -Seconds 3
  [System.Windows.Forms.SendKeys]::SendWait("^l"); Start-Sleep -Seconds 1
  [System.Windows.Forms.SendKeys]::SendWait("https://example.com"); Start-Sleep -Milliseconds 400
  [System.Windows.Forms.SendKeys]::SendWait("{ENTER}"); Start-Sleep -Seconds 8
  [System.Windows.Forms.SendKeys]::SendWait("^l"); Start-Sleep -Seconds 1
  [System.Windows.Forms.SendKeys]::SendWait("summarize this page for me"); Start-Sleep -Milliseconds 400
  [System.Windows.Forms.SendKeys]::SendWait("{ENTER}")
  Start-Sleep -Seconds 10
  Shot "21-chat-on-page"
} else { Write-Host "[smoke] run5: no window"; $status = 1 }
Stop-Browser $p5 "run5"
Stop-Process -Id $mock.Id -Force -ErrorAction SilentlyContinue
Remove-Item Env:MYTHBROWSE_SMOKE_KEY

Write-Host "----- mock-chat.log"; Get-Content $mockLog -ErrorAction SilentlyContinue | Write-Host
$ml = Get-Content $mockLog -Raw -ErrorAction SilentlyContinue
if ($ml -notmatch "POST chat auth=ok") { Write-Host "[smoke] run5: the browser never reached the mock model"; $status = 1 }
if ($ml -match "tools=[1-9]") { Write-Host "[smoke] run5: the open build sent tool definitions"; $status = 1 }
if ($ml -match "auth=BAD") { Write-Host "[smoke] run5: the API key did not reach the server"; $status = 1 }
$chats = ([regex]::Matches([string]$ml, "POST chat auth=ok")).Count
if ($chats -lt 2) { Write-Host "::warning::run5: the question asked on a web page did not reach the model ($chats chat request(s))" }
$leaks = Get-ChildItem $OutDir -Recurse -File -Include *.log, *.txt, *.toml | Select-String -SimpleMatch $key
if ($leaks) { Write-Host "[smoke] run5: API KEY LEAKED into: $($leaks.Path | Sort-Object -Unique)"; $status = 1 }
else { Write-Host "[smoke] run5: API key not found in any log (good)" }
Show-Logs "run5"

Get-ChildItem $OutDir -Filter "run*-std*.log" | ForEach-Object {
  Write-Host "===== $($_.Name) (last 60 lines)"
  Get-Content $_.FullName -Tail 60
}
exit $status
