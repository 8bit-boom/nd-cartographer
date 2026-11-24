# Download the latest prebuilt Windows server binary into target\release\ndcart.exe.
# Used by install.bat / update.bat when Rust is not installed. No toolchain needed.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)   # scripts\windows -> repo root
$dest = Join-Path $root 'target\release'
New-Item -ItemType Directory -Force -Path $dest | Out-Null

$headers = @{ 'User-Agent' = 'ndcart-windows-install' }
$rel = Invoke-RestMethod -Uri 'https://api.github.com/repos/8bit-boom/nd-cartographer/releases/latest' -Headers $headers
# The server zip is ndcart-vX-windows-x86_64.zip (not the ndcart-desktop-* installers).
$asset = $rel.assets | Where-Object { $_.name -match '^ndcart-v.*-windows-x86_64\.zip$' } | Select-Object -First 1
if (-not $asset) { throw 'No Windows server zip (ndcart-v*-windows-x86_64.zip) found in the latest release.' }

$tmp = Join-Path $env:TEMP ('ndcart-dl-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
    $zip = Join-Path $tmp 'ndcart.zip'
    Write-Host "Downloading $($asset.name) ($($rel.tag_name)) ..."
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -UseBasicParsing -Headers $headers
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $exe = Get-ChildItem -Path $tmp -Recurse -Filter 'ndcart.exe' | Select-Object -First 1
    if (-not $exe) { throw 'ndcart.exe was not found inside the release archive.' }
    Copy-Item -Path $exe.FullName -Destination (Join-Path $dest 'ndcart.exe') -Force
    Write-Host "Installed prebuilt ndcart.exe ($($rel.tag_name)) to $dest"
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}