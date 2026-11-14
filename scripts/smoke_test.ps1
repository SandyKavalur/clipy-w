# Smoke test for ClipYRS in test mode (F12 hotkey).
# 1. Start app in test mode
# 2. Seed clipboard with distinct values (builds history)
# 3. Press F12 to open the popup
# 4. Screenshot so the popup can be verified visually
param(
    [int]$Seconds = 2
)
$ErrorActionPreference = "Stop"

$exe = Join-Path $PSScriptRoot "..\target\release\clipyrs.exe"
$shot = Join-Path $PSScriptRoot "..\smoke_popup.png"

# Kill any previous instance.
Get-Process -Name clipyrs -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

$env:CLIPYRS_TEST = "1"
$proc = Start-Process -FilePath $exe -PassThru
Write-Host "started pid $($proc.Id)"
Start-Sleep -Seconds $Seconds

# Seed history: copy three distinct strings.
@("alpha-smoke-one", "beta-smoke-two", "gamma-smoke-three") | ForEach-Object {
    Set-Clipboard -Value $_
    Start-Sleep -Milliseconds 400
}
Write-Host "seeded clipboard history"

# F12 opens the popup.  SendKeys to the active window is unreliable; use the
# low-level keyboard API via P/Invoke so it lands regardless of focus.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class K {
    [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
    public const int KEYEVENTF_KEYUP = 0x0002;
    public static void Tap(byte vk) {
        keybd_event(vk, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(50);
        keybd_event(vk, 0, KEYEVENTF_KEYUP, UIntPtr.Zero);
    }
}
"@
[K]::Tap(0x78)  # VK_F9
Start-Sleep -Milliseconds 800

# Screenshot the whole screen.
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
$bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
$bmp = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
$gfx = [System.Drawing.Graphics]::FromImage($bmp)
$gfx.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
$bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
$gfx.Dispose(); $bmp.Dispose()
Write-Host "screenshot saved to $shot"

Write-Host "DONE" -ForegroundColor Green
