# Start the Pumpkin PC agent (screen clips while you Go Live). Ctrl+C stops it.
# Needs ffmpeg (winget install Gyan.FFmpeg) and PUMPKIN_AGENT_TOKEN in .env (same value as on the server).
$root = Split-Path $PSScriptRoot
& "$root\.venv\Scripts\python.exe" "$PSScriptRoot\pumpkin_agent.py"
