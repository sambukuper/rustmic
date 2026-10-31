# Installs the latest rustmic release for the current user on Windows.
$ErrorActionPreference = "Stop"

$repo = "sambukuper/rustmic"
$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest"
$asset = $release.assets | Where-Object { $_.name -like "*windows-x64.zip" } | Select-Object -First 1
if (-not $asset) {
    throw "no windows asset found in the latest release"
}

$dest = Join-Path $env:LOCALAPPDATA "rustmic"
$tmp = Join-Path $env:TEMP ("rustmic-" + [guid]::NewGuid().ToString("N"))

Write-Host "Downloading $($asset.name)..."
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile "$tmp.zip"
if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
Expand-Archive -Path "$tmp.zip" -DestinationPath $dest -Force
Remove-Item "$tmp.zip" -Force

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$dest*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$dest", "User")
    Write-Host "Added $dest to the user PATH."
}

Write-Host ""
Write-Host "rustmic installed to $dest"
Write-Host "Open a new terminal, then: rustmic-gui  (window)  or  rustmic --help  (command line)"
