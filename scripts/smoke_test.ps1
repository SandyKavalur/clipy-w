# Smoke test for Clipy-W in test mode (F9 hotkey).
# 1. Start app in test mode
# 2. Seed clipboard with distinct values (builds history)
# 3. Press F9 to open the popup
# 4. Screenshot so the popup can be verified visually
param(
    [int]$Seconds = 2
)
$ErrorActionPreference = "Stop"

$exe = Join-Path $PSScriptRoot "..\target\release\clipy-w.exe"
$shot = Join-Path $PSScriptRoot "..\smoke_popup.png"

# Kill any previous instance.
Get-Process -Name clipy-w,clipyrs -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

$env:CLIPY_W_TEST = "1"
$proc = Start-Process -FilePath $exe -PassThru
Write-Host "started pid $($proc.Id)"
Start-Sleep -Seconds $Seconds

# Seed history: copy three distinct strings.
$tag = (Get-Date).ToString("HHmmss")
@("alpha-$tag", "beta-$tag", "gamma-$tag") | ForEach-Object {
    Set-Clipboard -Value $_
    Start-Sleep -Milliseconds 400
}
Write-Host "seeded clipboard history"

# Seed history with a real bitmap and with HTML content so the image and
# formatted rows can be verified in the screenshot.  Set-Clipboard -Path would
# only put a file-drop list on the clipboard, so use the .NET Clipboard API.
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
$bmp = New-Object System.Drawing.Bitmap 64, 64
$gfx = [System.Drawing.Graphics]::FromImage($bmp)
$gfx.Clear([System.Drawing.Color]::CornflowerBlue)
$gfx.Dispose()
[System.Windows.Forms.Clipboard]::SetImage($bmp)
$bmp.Dispose()
Start-Sleep -Milliseconds 400
[System.Windows.Forms.Clipboard]::SetText("$tag-html-<b>bold smoke</b>", [System.Windows.Forms.TextDataFormat]::Html)
Start-Sleep -Milliseconds 400
# Seed history the way Explorer does when you copy an image file: a CF_HDROP
# file-drop list, not pixels.  The app must load the file and record a DIB.
$png = Join-Path $env:TEMP "clipy-smoke-$tag.png"
$seedBmp = New-Object System.Drawing.Bitmap 64, 64
$seedGfx = [System.Drawing.Graphics]::FromImage($seedBmp)
$seedGfx.Clear([System.Drawing.Color]::Gold)
$seedGfx.Dispose()
$seedBmp.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
$seedBmp.Dispose()
$files = New-Object System.Collections.Specialized.StringCollection
$null = $files.Add($png)
[System.Windows.Forms.Clipboard]::SetFileDropList($files)
Start-Sleep -Milliseconds 400
Write-Host "seeded image, rich content, and an Explorer-style file copy"

# F9 opens the popup. SendKeys to the active window is unreliable; use the
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
# Open the first history page so the screenshot shows the captured items.
[K]::Tap(0x27)  # VK_RIGHT
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

# Restore check: row 1 is the newest item, the Explorer-style file copy.
# Restoring it should put a CF_DIB back on the clipboard.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class P {
    [DllImport("user32.dll")] public static extern bool OpenClipboard(IntPtr h);
    [DllImport("user32.dll")] public static extern bool CloseClipboard();
    [DllImport("user32.dll")] public static extern uint EnumClipboardFormats(uint f);
}
"@
Start-Sleep -Milliseconds 300
[K]::Tap(0x0D)  # VK_RETURN -> restore row 1 ([Image] from the copied file)
Start-Sleep -Milliseconds 600
[P]::OpenClipboard([IntPtr]::Zero) | Out-Null
$hasDib = $false
$f = 0
while (($f = [P]::EnumClipboardFormats($f)) -ne 0) {
    if ($f -eq 8) { $hasDib = $true }
}
[P]::CloseClipboard() | Out-Null
if ($hasDib) {
    Write-Host "restore check: CF_DIB is back on the clipboard" -ForegroundColor Green
} else {
    Write-Host "restore check: FAILED, no CF_DIB on the clipboard" -ForegroundColor Red
}

Write-Host "DONE" -ForegroundColor Green
