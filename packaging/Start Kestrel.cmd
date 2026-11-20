@echo off
rem Kestrel launcher: asks for administrator rights once, then starts the service console, the tray and the app.
cd /d "%~dp0"
net session >nul 2>&1
if errorlevel 1 (
  powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
  exit /b
)
tasklist /fi "imagename eq kestrel-svc.exe" | find /i "kestrel-svc.exe" >nul
if errorlevel 1 (
  start "Kestrel service - close this window to stop it" cmd /k ""%~dp0kestrel-svc.exe" --console"
  timeout /t 2 /nobreak >nul
)
tasklist /fi "imagename eq kestrel-tray.exe" | find /i "kestrel-tray.exe" >nul || start "" "%~dp0kestrel-tray.exe"
start "" "%~dp0kestrel-ui.exe"
