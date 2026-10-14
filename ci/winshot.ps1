# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 JustMyth / Tan Ming Jing
#
# Capture the client area of a process's main window as PNG (used by the real-shell AI e2e,
# crates/e2e/src/shell.rs). -Move first sizes the window to 1440x900 at (120, 60) like the glass
# screenshots and brings it to the front. Exit 0 when the PNG was written.
#
# Usage: ci/winshot.ps1 -ProcessId <pid> -Out <file.png> [-Move]
param(
  [Parameter(Mandatory = $true)] [int] $ProcessId,
  [Parameter(Mandatory = $true)] [string] $Out,
  [switch] $Move
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class WS {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int size);
  public static int[] Client(IntPtr h) {
    RECT r; GetClientRect(h, out r); POINT p = new POINT(); ClientToScreen(h, ref p);
    int x0 = p.X, y0 = p.Y, x1 = p.X + (r.Right - r.Left), y1 = p.Y + (r.Bottom - r.Top);
    RECT f;
    if (DwmGetWindowAttribute(h, 9, out f, Marshal.SizeOf(typeof(RECT))) == 0 && f.Right > f.Left) {
      x0 = Math.Max(x0, f.Left); y0 = Math.Max(y0, f.Top); x1 = Math.Min(x1, f.Right); y1 = Math.Min(y1, f.Bottom);
    }
    return new int[] { x0, y0, x1 - x0, y1 - y0 };
  }
}
"@
try { [WS]::SetProcessDpiAwarenessContext([IntPtr]::new(-4)) | Out-Null } catch { }
$p = Get-Process -Id $ProcessId -ErrorAction Stop
$h = $p.MainWindowHandle
if ($h -eq [IntPtr]::Zero) { Write-Host "no main window"; exit 1 }
if ($Move) { [WS]::MoveWindow($h, 120, 60, 1440, 900, $true) | Out-Null; Start-Sleep -Milliseconds 600 }
[WS]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 150
$c = [WS]::Client($h)
if ($c[2] -lt 100 -or $c[3] -lt 100) { Write-Host "window too small: $($c -join ',')"; exit 1 }
$bmp = New-Object System.Drawing.Bitmap $c[2], $c[3]
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($c[0], $c[1], 0, 0, $bmp.Size)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
exit 0
