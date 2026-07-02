# Установщик LChat для Windows 11 (PowerShell).
# Собирает release, ставит в профиль пользователя, делает ярлык в меню «Пуск»
# и (при запуске от администратора) открывает порты в брандмауэре.
#
# Запуск:  правой кнопкой -> «Выполнить с помощью PowerShell»
#   или:   powershell -ExecutionPolicy Bypass -File install.ps1

$ErrorActionPreference = "Stop"
Set-Location -Path $PSScriptRoot

# 1) Проверка Rust.
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "Не найден Rust (cargo). Установи с https://rust-lang.org (rustup) и повтори." -ForegroundColor Red
    exit 1
}

Write-Host "==> Сборка (release)..." -ForegroundColor Cyan
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "Сборка не удалась" }

# 2) Копирование бинарника в профиль пользователя.
$destDir = Join-Path $env:LOCALAPPDATA "Programs\LChat"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$exe = Join-Path $destDir "lchat.exe"
Copy-Item "target\release\lchat.exe" $exe -Force
Write-Host "==> Установлено: $exe"

# 3) Ярлык в меню «Пуск».
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
$lnk = Join-Path $startMenu "LChat.lnk"
$ws = New-Object -ComObject WScript.Shell
$sc = $ws.CreateShortcut($lnk)
$sc.TargetPath = $exe
$sc.WorkingDirectory = $destDir
$sc.Description = "Локальный P2P-чат по локальной сети"
$sc.Save()
Write-Host "==> Ярлык в меню Пуск: LChat"

# 4) Брандмауэр (нужны права администратора).
$admin = ([Security.Principal.WindowsPrincipal] `
    [Security.Principal.WindowsIdentity]::GetCurrent()`
    ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

if ($admin) {
    Write-Host "==> Открываю порты в брандмауэре (9009/TCP, 9010/UDP)..."
    netsh advfirewall firewall add rule name="LChat TCP 9009" dir=in action=allow protocol=TCP localport=9009 | Out-Null
    netsh advfirewall firewall add rule name="LChat UDP 9010" dir=in action=allow protocol=UDP localport=9010 | Out-Null
} else {
    Write-Host "Правила брандмауэра пропущены (нет прав администратора)." -ForegroundColor Yellow
    Write-Host "При первом запуске Защитник спросит доступ — разреши для ЧАСТНЫХ сетей."
}

Write-Host ""
Write-Host "Готово. Запусти LChat из меню Пуск." -ForegroundColor Green
Write-Host "Совет: для миниатюр видео поставь ffmpeg и добавь его в PATH."
