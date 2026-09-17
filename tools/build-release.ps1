<#
.SYNOPSIS
    Релизная сборка MH Monitoring (PLAN.md §6/P9): служба, потом оверлей со встроенной службой.

.DESCRIPTION
    Порядок важен: MH-Monitoring.exe встраивает MH-Monitoring-Service.exe при сборке
    (crates/ui/build.rs), поэтому служба собирается первой, а путь к ней передаётся явно через
    MH_SERVICE_EXE. `cargo build --workspace` порядка не гарантирует и мог бы встроить старую
    службу.

    Обе сборки идут с --locked: версии зависимостей берутся ровно из Cargo.lock. Новая версия
    крейта, выпущенная час назад (так распространяются вредоносные версии), в релиз не попадёт,
    пока Cargo.lock не обновят осознанно.

    Результат — папка target\dist: MH-Monitoring.exe (весь дистрибутив) и SHA256SUMS.txt.

.EXAMPLE
    .\tools\build-release.ps1
#>
param(
    [string]$OutDir = "target\dist"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    cargo build --release --locked -p mh-service
    if ($LASTEXITCODE -ne 0) { throw "сборка службы не удалась" }
    $service = Join-Path $root "target\release\MH-Monitoring-Service.exe"

    $env:MH_SERVICE_EXE = $service
    try {
        cargo build --release --locked -p mh-ui
        if ($LASTEXITCODE -ne 0) { throw "сборка оверлея не удалась" }
    } finally {
        Remove-Item Env:MH_SERVICE_EXE
    }
    $overlay = Join-Path $root "target\release\MH-Monitoring.exe"

    # Встроенная служба делает оверлей заведомо больше её самой; иначе встроился пустой файл.
    if ((Get-Item $overlay).Length -le (Get-Item $service).Length) {
        throw "служба не встроена в MH-Monitoring.exe"
    }

    New-Item -ItemType Directory -Force $OutDir | Out-Null
    Copy-Item $overlay $OutDir -Force
    $sums = Get-ChildItem $OutDir -Filter *.exe | ForEach-Object {
        "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name
    }
    Set-Content -Path (Join-Path $OutDir "SHA256SUMS.txt") -Value $sums -Encoding ascii
    $sums
} finally {
    Pop-Location
}
