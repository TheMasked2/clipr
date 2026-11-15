@echo off
rem Pumpkin PC agent: puts your screen in clips while you Go Live. Keep this window open; close it to stop.
title Pumpkin agent
cd /d "%~dp0"
set "PATH=%PATH%;%LOCALAPPDATA%\Microsoft\WinGet\Links;%USERPROFILE%\.local\bin"
where uv >nul 2>nul || (
  echo Installing uv, which runs the agent...
  winget install -e --id astral-sh.uv --accept-source-agreements --accept-package-agreements
)
where ffmpeg >nul 2>nul || if not exist "%LOCALAPPDATA%\Microsoft\WinGet\Packages\Gyan.FFmpeg*" (
  echo Installing ffmpeg, which records the screen...
  winget install -e --id Gyan.FFmpeg --accept-source-agreements --accept-package-agreements
)
uv run --quiet --python 3.12 pumpkin_agent.py
pause
