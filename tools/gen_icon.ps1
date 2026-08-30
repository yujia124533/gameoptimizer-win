# 生成 GameOptimizer 图标：蓝色圆角方块 + 金色闪电（多尺寸 PNG 压缩 .ico）
Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'

function New-RoundRectPath($rect, $r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddArc($rect.X, $rect.Y, $r * 2, $r * 2, 180, 90)
    $p.AddArc($rect.Right - $r * 2, $rect.Y, $r * 2, $r * 2, 270, 90)
    $p.AddArc($rect.Right - $r * 2, $rect.Bottom - $r * 2, $r * 2, $r * 2, 0, 90)
    $p.AddArc($rect.X, $rect.Bottom - $r * 2, $r * 2, $r * 2, 90, 90)
    $p.CloseFigure()
    return $p
}

function Draw-Icon([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.Clear([System.Drawing.Color]::Transparent)

    $pad = [int]($size * 0.04)
    $rect = New-Object System.Drawing.Rectangle($pad, $pad, $size - 2 * $pad, $size - 2 * $pad)
    $rr = [int]($size * 0.20)
    $path = New-RoundRectPath $rect $rr

    $c1 = [System.Drawing.Color]::FromArgb(255, 26, 42, 108)     # 深蓝
    $c2 = [System.Drawing.Color]::FromArgb(255, 61, 108, 181)    # 亮蓝
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, $c1, $c2, 90)
    $g.FillPath($brush, $path)

    # 金色闪电
    $s = $size
    $pts = @(
        (New-Object System.Drawing.Point([int]($s * 0.60), [int]($s * 0.20))),
        (New-Object System.Drawing.Point([int]($s * 0.36), [int]($s * 0.55))),
        (New-Object System.Drawing.Point([int]($s * 0.49), [int]($s * 0.55))),
        (New-Object System.Drawing.Point([int]($s * 0.40), [int]($s * 0.80))),
        (New-Object System.Drawing.Point([int]($s * 0.68), [int]($s * 0.45))),
        (New-Object System.Drawing.Point([int]($s * 0.54), [int]($s * 0.45)))
    )
    $bolt = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 255, 211, 105))
    $g.FillPolygon($bolt, $pts)

    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    $data = $ms.ToArray()
    $ms.Dispose()
    return (, $data)
}

$sizes = @(16, 32, 48, 256)
$images = @()
foreach ($sz in $sizes) { $images += , $sz; $images += , (Draw-Icon $sz) }

$ms = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter($ms)
$bw.Write([uint16]0)                      # reserved
$bw.Write([uint16]1)                      # type: icon
$bw.Write([uint16]$sizes.Count)           # count
$offset = 6 + 16 * $sizes.Count
foreach ($i in 0..($sizes.Count - 1)) {
    $sz = $sizes[$i]
    $png = $images[$i * 2 + 1]
    $w = if ($sz -ge 256) { 0 } else { $sz }
    $bw.Write([byte]$w)        # width
    $bw.Write([byte]$w)        # height
    $bw.Write([byte]0)         # color count
    $bw.Write([byte]0)         # reserved
    $bw.Write([uint16]1)       # planes
    $bw.Write([uint16]32)      # bpp
    $bw.Write([uint32]$png.Length)
    $bw.Write([uint32]$offset)
    $offset += $png.Length
}
foreach ($i in 0..($sizes.Count - 1)) { $bw.Write($images[$i * 2 + 1]) }
$bw.Flush()

$out = "E:\deekseek  harness operrating area\GameOptimizer\resources\icon.ico"
[System.IO.File]::WriteAllBytes($out, $ms.ToArray())
$bw.Dispose(); $ms.Dispose()
Write-Host "icon.ico 生成: $((Get-Item $out).Length) 字节, $($sizes.Count) 个尺寸"
