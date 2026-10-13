#!/usr/bin/env bash
# Asks a running MacWin.app to stop gracefully (SIGINT, like Ctrl+C).
set -euo pipefail
if pkill -INT -f "MacWin.app/Contents/MacOS/macwin"; then
    echo "stop requested"
else
    echo "MacWin.app is not running"
fi
