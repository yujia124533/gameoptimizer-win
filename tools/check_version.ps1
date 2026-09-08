# GameOptimizer version consistency check (run before release / before tag)
# Verifies src/version.h against resources/resource.rc, resources/gui_resource.rc and README.md.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$ver = $null
$m = Select-String -Path "$root\src\version.h" -Pattern 'GOPT_VERSION_STR "([0-9.]+)"'
if ($m) { $ver = $m.Matches[0].Groups[1].Value }
if (-not $ver) { Write-Host "FAIL: cannot read GOPT_VERSION_STR"; exit 1 }

$fail = $false
$rcVer = $ver.Replace('.', ',') + ',0'

foreach ($f in @("$root\resources\resource.rc", "$root\resources\gui_resource.rc")) {
    $t = Get-Content -Raw $f
    if (-not ($t.Contains("FILEVERSION     $rcVer"))) {
        Write-Host "MISS: $f -> FILEVERSION $rcVer"; $fail = $true
    }
    if (-not ($t.Contains("VALUE `"FileVersion`",      `"$ver`""))) {
        Write-Host "MISS: $f -> FileVersion string $ver"; $fail = $true
    }
    if (-not ($t.Contains("VALUE `"ProductVersion`",   `"$ver`""))) {
        Write-Host "MISS: $f -> ProductVersion string $ver"; $fail = $true
    }
}

$readme = Get-Content -Raw "$root\README.md"
if (-not ($readme.Contains("v$ver ") -and $readme.Contains("##"))) {
    Write-Host "MISS: README.md -> changelog v$ver"; $fail = $true
}

if ($fail) { Write-Host "RESULT: FAIL (version inconsistency)"; exit 1 }
Write-Host "RESULT: PASS (all version markers = v$ver)"