@echo off
setlocal

set "INSTALLER_URL=https://herdr.dev/install.ps1"
set "CURL_PROTOCOL=--proto =https --tlsv1.2"
if defined HERDR_INSTALLER_URL (
    set "INSTALLER_URL=%HERDR_INSTALLER_URL%"
    set "CURL_PROTOCOL=--proto =http,https"
)
set "INSTALLER_PATH=%TEMP%\herdr-install-%RANDOM%-%RANDOM%.ps1"

curl.exe --fail --silent --show-error --location --connect-timeout 30 --speed-limit 1024 --speed-time 30 %CURL_PROTOCOL% --output "%INSTALLER_PATH%" -- "%INSTALLER_URL%"
if errorlevel 1 (
    del /f /q "%INSTALLER_PATH%" >nul 2>&1
    exit /b 1
)

powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%INSTALLER_PATH%" %*
set "INSTALLER_EXIT=%ERRORLEVEL%"
del /f /q "%INSTALLER_PATH%" >nul 2>&1
exit /b %INSTALLER_EXIT%
