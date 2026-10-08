@echo off
setlocal
title LanDesk Connect to Mac
set "MAC_HOST="
set "MAC_USER="
set /p "MAC_HOST=Mac IP or hostname: "
set /p "MAC_USER=Mac SSH username: "
if not defined MAC_HOST exit /b 1
if not defined MAC_USER exit /b 1
where ssh.exe >nul 2>nul
if errorlevel 1 (
  echo Windows OpenSSH Client is required. Enable it in Windows Optional Features.
  pause
  exit /b 1
)
powershell.exe -NoProfile -Command "$c=New-Object Net.Sockets.TcpClient; try { $c.Connect('127.0.0.1',17890); exit 1 } catch { exit 0 } finally { $c.Dispose() }"
if errorlevel 1 (
  echo Port 17890 is already in use. Close the previous LanDesk connection first.
  pause
  exit /b 1
)
echo Keep this window open while using LanDesk.
echo Enter your Mac SSH password below. The browser opens automatically.
echo In the browser, enter the connection code shown in LanDesk on your Mac.
echo Press Ctrl+C or close this window to stop the encrypted tunnel.
echo.
start "" /b powershell.exe -NoProfile -Command "for($i=0;$i -lt 180;$i++) { $c=New-Object Net.Sockets.TcpClient; $ready=$false; try { $c.Connect('127.0.0.1',17890); $ready=$true } catch {} finally { $c.Dispose() }; if($ready) { Start-Process 'http://127.0.0.1:17890'; exit }; Start-Sleep -Seconds 1 }"
ssh.exe -NT -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=2 -L 127.0.0.1:17890:127.0.0.1:17890 "%MAC_USER%@%MAC_HOST%"
echo.
echo SSH connection closed.
pause
