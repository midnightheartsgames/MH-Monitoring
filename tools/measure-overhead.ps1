<#
.SYNOPSIS
    Замер собственной нагрузки MH Monitoring (PLAN.md §6/P7): CPU, память, дескрипторы, потоки.

.DESCRIPTION
    Раз в -Interval секунд снимает счётчики процессов MH-Monitoring (оверлей и служба) и
    PresentMon и в конце печатает среднее и максимум по каждому процессу.

    Прав администратора не нужно: счётчики производительности видны и для процессов SYSTEM.
    Классы WMI называются по-английски на любой локализации Windows, в отличие от путей
    Get-Counter.

    CPU приведён к доле всей машины: 100 % — все логические ядра заняты.

.EXAMPLE
    .\tools\measure-overhead.ps1 -Seconds 120 -Label "простой"
    .\tools\measure-overhead.ps1 -Seconds 300 -Label "Cyberpunk 2077, 1440p"
#>
param(
    [int]$Seconds = 60,
    [int]$Interval = 2,
    [string]$Label = "",
    [string[]]$Names = @("MH-Monitoring*", "PresentMon*")
)

$ErrorActionPreference = "Stop"
$cores = [Environment]::ProcessorCount
$filter = ($Names | ForEach-Object { "Name like '$($_.Replace('*', '%'))'" }) -join " or "

# Какой процесс чем является. Командную строку процесса SYSTEM без прав не прочитать, поэтому
# службу узнаём по PID из диспетчера служб — он виден всем.
$roles = @{}
function Get-Role([uint32]$ProcessId) {
    if (-not $roles.ContainsKey($ProcessId)) {
        $process = Get-CimInstance Win32_Process -Filter "ProcessId = $ProcessId" -ErrorAction SilentlyContinue
        $service = Get-CimInstance Win32_Service -Filter "Name = 'MHMonitor'" -ErrorAction SilentlyContinue
        $role = if ($service -and $service.ProcessId -eq $ProcessId) { "служба" }
            elseif (-not $process) { "?" }
            elseif ($process.Name -like "PresentMon*") { "PresentMon" }
            elseif ($process.CommandLine -like "*--serv*") { "служба" }
            else { "оверлей" }
        $roles[$ProcessId] = $role
    }
    $roles[$ProcessId]
}

$samples = [System.Collections.Generic.List[object]]::new()
$deadline = (Get-Date).AddSeconds($Seconds)
Write-Host "Замер $Seconds с, шаг $Interval с, логических ядер: $cores. $Label"
# Первый снимок PercentProcessorTime у WMI бывает нулевым — пропускаем его.
$null = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter $filter
Start-Sleep -Seconds $Interval

while ((Get-Date) -lt $deadline) {
    foreach ($row in Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter $filter) {
        $samples.Add([pscustomobject]@{
            Pid     = $row.IDProcess
            Role    = Get-Role $row.IDProcess
            Cpu     = $row.PercentProcessorTime / $cores
            PrivMB  = $row.PrivateBytes / 1MB
            WsMB    = $row.WorkingSetPrivate / 1MB
            Handles = $row.HandleCount
            Threads = $row.ThreadCount
        })
    }
    Start-Sleep -Seconds $Interval
}

if ($samples.Count -eq 0) {
    Write-Warning "Процессы не найдены: $($Names -join ', ')"
    exit 1
}

Write-Host "CPU — среднее и максимум, доля всей машины; Priv — выделенная память (среднее и максимум);"
Write-Host "WS — собственная рабочая память; Дескр. и Потоки — максимум за замер."
$samples | Group-Object Role, Pid | ForEach-Object {
    $group = $_.Group
    $stat = { param($name) $group | Measure-Object -Property $name -Average -Maximum }
    $cpu = & $stat Cpu; $priv = & $stat PrivMB; $ws = & $stat WsMB
    $handles = & $stat Handles; $threads = & $stat Threads
    [pscustomobject]@{
        "Процесс"   = "$($group[0].Role) ($($group[0].Pid))"
        "N"         = $group.Count
        "CPU %"     = [math]::Round($cpu.Average, 2)
        "CPU макс"  = [math]::Round($cpu.Maximum, 2)
        "Priv МБ"   = [math]::Round($priv.Average, 1)
        "Priv макс" = [math]::Round($priv.Maximum, 1)
        "WS МБ"     = [math]::Round($ws.Average, 1)
        "Дескр."    = [int]$handles.Maximum
        "Потоки"    = [int]$threads.Maximum
    }
} | Sort-Object "Процесс" | Format-Table -AutoSize
