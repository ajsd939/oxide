# Draws the Rust IDE app icon: rounded-square blue gradient with a
# terminal prompt (">_") mark. Output: icons/icon-source.png (1024x1024,
# transparent corners) - feed it to `cargo tauri icon` to get the .ico set.
# Usage: powershell -ExecutionPolicy Bypass -File scripts/make-icon.ps1

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$size = 1024
$bmp = New-Object System.Drawing.Bitmap($size, $size)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)

function New-RoundedRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddArc($x, $y, 2 * $r, 2 * $r, 180, 90)
    $p.AddArc($x + $w - 2 * $r, $y, 2 * $r, 2 * $r, 270, 90)
    $p.AddArc($x + $w - 2 * $r, $y + $h - 2 * $r, 2 * $r, 2 * $r, 0, 90)
    $p.AddArc($x, $y + $h - 2 * $r, 2 * $r, 2 * $r, 90, 90)
    $p.CloseFigure()
    return $p
}

# Rounded square with vertical gradient.
$tile = New-RoundedRect 64 64 896 896 220
$gradient = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    (New-Object System.Drawing.Point(0, 64)),
    (New-Object System.Drawing.Point(0, 960)),
    [System.Drawing.Color]::FromArgb(255, 0x5C, 0x9D, 0xFF),
    [System.Drawing.Color]::FromArgb(255, 0x1F, 0x4E, 0xC0))
$g.FillPath($gradient, $tile)

# Soft top sheen: a translucent white rounded band clipped to the tile.
$clip = $g.Clip
$g.SetClip($tile)
$sheen = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    (New-Object System.Drawing.Point(0, 64)),
    (New-Object System.Drawing.Point(0, 420)),
    [System.Drawing.Color]::FromArgb(70, 255, 255, 255),
    [System.Drawing.Color]::FromArgb(0, 255, 255, 255))
$g.FillRectangle($sheen, 0, 0, $size, 420)
$g.Clip = $clip

# Terminal prompt: ">" chevron in white...
$white = New-Object System.Drawing.Pen([System.Drawing.Color]::White, 96)
$white.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
$white.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
$white.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
$g.DrawLine($white, 268, 296, 496, 512)
$g.DrawLine($white, 496, 512, 268, 728)

# ...and "_" cursor in pale blue.
$pale = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 0xC7, 0xDE, 0xFF), 96)
$pale.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
$pale.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
$g.DrawLine($pale, 600, 728, 804, 728)

$g.Dispose()

$out = Join-Path (Split-Path -Parent $PSScriptRoot) "icons\icon-source.png"
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Host "Wrote $out"
