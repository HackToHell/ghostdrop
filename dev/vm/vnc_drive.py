#!/usr/bin/env python3
"""Drive a VM desktop over one persistent VNC connection (like a real user).

usage: vnc_drive.py HOST::PORT [PASSWORD] < script

script lines:
  key ctrl-grave        press a key combo (vncdotool names: ctrl, shift, alt, enter, bsp, tab, pgup, pgdn, f1..)
  type some text        type literal text
  sleep 0.5
  move X Y              move the pointer
  click N               click mouse button N (1 left, 2 middle, 3 right, 4/5 wheel)
  down N / up N         press / release mouse button N
  capture /path.png     screenshot
  # comment
"""
import sys
import time

from vncdotool import api


def main():
    server = sys.argv[1]
    password = sys.argv[2] if len(sys.argv) > 2 and sys.argv[2] else None
    client = api.connect(server, password=password)
    try:
        client.timeout = 30
        time.sleep(1.0)  # let the server create its virtual input devices
        for raw in sys.stdin:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            cmd, _, arg = line.partition(" ")
            if cmd == "key":
                client.keyPress(arg)
            elif cmd == "type":
                for ch in arg:
                    client.keyPress(ch)
                    time.sleep(0.01)
            elif cmd == "sleep":
                time.sleep(float(arg))
            elif cmd == "move":
                x, y = arg.split()
                client.mouseMove(int(x), int(y))
            elif cmd == "click":
                client.mousePress(int(arg))
            elif cmd == "down":
                client.mouseDown(int(arg))
            elif cmd == "up":
                client.mouseUp(int(arg))
            elif cmd == "capture":
                client.refreshScreen()
                client.captureScreen(arg)
            else:
                print(f"unknown command: {line}", file=sys.stderr)
            time.sleep(0.05)
    finally:
        client.disconnect()
        api.shutdown()


if __name__ == "__main__":
    main()
