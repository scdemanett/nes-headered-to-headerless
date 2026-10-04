# Draws the 1024px source icon; run `npx tauri icon build/icon.png` afterwards.
Add-Type -AssemblyName System.Drawing

$size = 1024
$bmp = New-Object System.Drawing.Bitmap $size, $size
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = 'AntiAlias'
$g.TextRenderingHint = 'AntiAliasGridFit'
$g.Clear([System.Drawing.Color]::Transparent)

function New-RoundedRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
  $p = New-Object System.Drawing.Drawing2D.GraphicsPath
  $d = $r * 2
  $p.AddArc($x, $y, $d, $d, 180, 90)
  $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
  $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
  $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
  $p.CloseFigure()
  return $p
}

# Tile
$tile = New-RoundedRect 48 48 928 928 180
$g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 18, 21, 28))), $tile)

# The 16-byte header being cut away (red strip), and the payload that stays.
$header = New-RoundedRect 176 236 672 120 36
$g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 226, 60, 60))), $header)
$pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 18, 21, 28)), 18
$g.DrawLine($pen, 236, 326, 788, 266)

$payload = New-RoundedRect 176 404 672 384 48
$g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 222, 226, 233))), $payload)

$font = New-Object System.Drawing.Font 'Segoe UI', 150, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
$fmt = New-Object System.Drawing.StringFormat
$fmt.Alignment = 'Center'
$fmt.LineAlignment = 'Center'
$rect = New-Object System.Drawing.RectangleF 176, 404, 672, 384
$g.DrawString('.unh', $font, (New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 18, 21, 28))), $rect, $fmt)

$out = Join-Path $PSScriptRoot '..\build\icon.png'
New-Item -ItemType Directory -Force (Split-Path $out) | Out-Null
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Host "Wrote $out"
