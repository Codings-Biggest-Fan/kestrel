# Starts the service (mock EC), tray and main window, then fails if their combined
# private working set is over the limit.
param([string]$Bin = "target/release", [int]$LimitMB = 50)
$ErrorActionPreference = "Stop"
$svc  = Start-Process "$Bin/kestrel-svc.exe" -ArgumentList "--mock","15MMIMS1.103" -PassThru -WindowStyle Hidden
Start-Sleep 2
$tray = Start-Process "$Bin/kestrel-tray.exe" -PassThru
$ui   = Start-Process "$Bin/kestrel-ui.exe" -PassThru
Start-Sleep 10
$total = 0
foreach ($p in @($svc, $tray, $ui)) {
  $p.Refresh()
  if ($p.HasExited) { throw "$($p.ProcessName) exited early (code $($p.ExitCode))" }
  $c = Get-Counter "\Process($($p.ProcessName))\Working Set - Private" -ErrorAction SilentlyContinue
  $mb = if ($c) { $c.CounterSamples[0].CookedValue / 1MB } else { $p.WorkingSet64 / 1MB }
  "{0,-14} {1,6:N1} MB" -f $p.ProcessName, $mb
  $total += $mb
}
"{0,-14} {1,6:N1} MB (limit $LimitMB MB)" -f "total", $total
@($svc, $tray, $ui) | Stop-Process -Force -ErrorAction SilentlyContinue
if ($total -gt $LimitMB) { throw "RAM budget exceeded" }
