# 把 assets/icon.png (任意分辨率) 缩到 32x32，覆盖 assets/tray-icon.png。
# 用法：在仓库根目录运行 `pwsh scripts/regen-tray-icon.ps1` 或 `powershell -ExecutionPolicy Bypass -File scripts\regen-tray-icon.ps1`
# 缩放用 HighQualityBicubic，保留 alpha 通道，背景透明。

Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$srcPath = Join-Path $root "assets\icon.png"
$dstPath = Join-Path $root "assets\tray-icon.png"

if (-not (Test-Path $srcPath)) {
    Write-Error "找不到源文件：$srcPath"
    exit 1
}

$src = [System.Drawing.Image]::FromFile($srcPath)
try {
    $bmp = New-Object System.Drawing.Bitmap(32, 32, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.DrawImage($src, 0, 0, 32, 32)
    $g.Dispose()
    $bmp.Save($dstPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
} finally {
    $src.Dispose()
}

$srcSize = (Get-Item $srcPath).Length
$dstSize = (Get-Item $dstPath).Length
Write-Output "icon.png ($($src.Width)x$($src.Height)): $srcSize bytes -> tray-icon.png (32x32): $dstSize bytes"
Write-Output "记得 cargo build --release 重新编译生效"
