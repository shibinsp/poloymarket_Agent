"""Ask Windows not to sleep (and keep the screen on) until a given time, then exit.
Changes no settings: the request ends with this process.

On this Modern Standby laptop the system sleeps soon after the display turns
off, which freezes the agent, so the display is asked for too. Closing the lid
still sleeps the laptop.

Usage: pythonw tools/stay_awake.py 2026-11-14T03:30+05:30
"""
import ctypes
import sys
import time
from datetime import datetime, timezone

ES_CONTINUOUS = 0x80000000
ES_SYSTEM_REQUIRED = 0x00000001
ES_DISPLAY_REQUIRED = 0x00000002
UNTIL = datetime.fromisoformat(sys.argv[1]).astimezone(timezone.utc)

try:
    while datetime.now(timezone.utc) < UNTIL:
        # Re-asserted each minute; the flags hold while this thread lives.
        ctypes.windll.kernel32.SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)
        time.sleep(60)
finally:
    ctypes.windll.kernel32.SetThreadExecutionState(ES_CONTINUOUS)
