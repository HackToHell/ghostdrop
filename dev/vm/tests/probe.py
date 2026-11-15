import os, sys, termios, tty, select, time, json
fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
out = {}
def query(seq, wait=0.3):
    os.write(1, seq.encode())
    buf = b""; end = time.time() + wait
    while time.time() < end:
        r,_,_ = select.select([fd],[],[],0.05)
        if r: buf += os.read(fd, 4096)
    return buf
try:
    tty.setraw(fd)
    out["DA1"] = query("\x1b[c")
    out["XTVERSION"] = query("\x1b[>0q")
    out["sync_2026"] = query("\x1b[?2026$p")
    out["kitty_kbd_query"] = query("\x1b[?u")
    out["kitty_graphics"] = query("\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\")
    out["color_osc11"] = query("\x1b]11;?\x07")
    out["osc52_write"] = query("\x1b]52;c;" + __import__('base64').b64encode(b"ghostdrop-osc52").decode() + "\x07", 0.2)
    # Kitty keyboard: push flags 1|8 (disambiguate + report all keys as escapes)
    os.write(1, b"\x1b[>9u")
    os.write(1, b"\x1b[?1004h")  # focus events
    open("/tmp/probe-ready","w").write("1")
    buf = b""; end = time.time() + 6
    while time.time() < end:
        r,_,_ = select.select([fd],[],[],0.05)
        if r: buf += os.read(fd, 4096)
    out["keys_kitty"] = buf
    os.write(1, b"\x1b[<u\x1b[?1004l")
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
json.dump({k: v.decode('latin1') for k,v in out.items()}, open("/tmp/probe.json","w"), indent=1)
print("PROBE DONE")
