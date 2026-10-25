# Vdisplay installer
#
#   irm https://raw.githubusercontent.com/visiuun/vdisplay/main/install.ps1 | iex
#
# Downloads the latest release into %LOCALAPPDATA%\Programs\Vdisplay, adds Start menu and
# desktop shortcuts, and starts the app. Run it again to update. No administrator rights needed.
# To remove: & ([scriptblock]::Create((irm https://raw.githubusercontent.com/visiuun/vdisplay/main/install.ps1))) -Uninstall

param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repo = 'visiuun/vdisplay'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\Vdisplay'
$exe = Join-Path $dir 'Vdisplay.exe'
$startMenu = Join-Path ([Environment]::GetFolderPath('Programs')) 'Vdisplay.lnk'
$desktop = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Vdisplay.lnk'

function Stop-Vdisplay {
    Get-Process vdisplay -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
}

if ($Uninstall) {
    Stop-Vdisplay
    Remove-Item $startMenu, $desktop -Force -ErrorAction SilentlyContinue
    Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Vdisplay' -ErrorAction SilentlyContinue
    Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host 'Vdisplay removed. Settings are kept in %APPDATA%\Vdisplay (delete that folder to remove them too).'
    return
}

Write-Host 'Fetching the latest Vdisplay release...'
$release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'vdisplay-installer' }
$asset = $release.assets | Where-Object { $_.name -like '*.exe' } | Select-Object -First 1
if (-not $asset) { throw "The latest release has no .exe attached." }

New-Item -ItemType Directory -Force $dir | Out-Null
$tmp = "$exe.download"
Invoke-WebRequest $asset.browser_download_url -OutFile $tmp -UseBasicParsing
Stop-Vdisplay
Move-Item $tmp $exe -Force
Unblock-File $exe -ErrorAction SilentlyContinue

$shell = New-Object -ComObject WScript.Shell
foreach ($path in $startMenu, $desktop) {
    $lnk = $shell.CreateShortcut($path)
    $lnk.TargetPath = $exe
    $lnk.WorkingDirectory = $dir
    $lnk.Description = 'Vdisplay case screen controller'
    $lnk.Save()
}

Write-Host "Installed Vdisplay $($release.tag_name) to $dir"
Start-Process $exe
