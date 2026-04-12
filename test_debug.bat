@echo off
set KIMI_API_KEY=%KIMI_API_KEY%
set pekobotDir=%USERPROFILE%\.pekobot
if exist %pekobotDir% rmdir /s /q %pekobotDir%

target\debug\pekobot.exe auth set kimi %KIMI_API_KEY% >nul 2>&1
target\debug\pekobot.exe agent create testagent --provider kimi >nul 2>&1

echo === Test ===
target\debug\pekobot.exe send testagent "test" --no-stream

target\debug\pekobot.exe agent delete testagent >nul 2>&1
