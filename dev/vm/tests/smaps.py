import sys, re, collections
pid = sys.argv[1]
groups = collections.Counter(); counts = collections.Counter()
cur = None
for line in open(f"/proc/{pid}/smaps"):
    if re.match(r"^[0-9a-f]+-[0-9a-f]+ ", line):
        parts = line.split()
        lo, hi = (int(x, 16) for x in parts[0].split("-"))
        name = parts[5] if len(parts) > 5 else ""
        size = hi - lo
        if name == "[heap]": cur = "[heap]"
        elif name.startswith("/"): cur = "file:" + name.split("/")[-1]
        elif name: cur = name
        elif size in (64 << 20, 128 << 20) or (size % (64 << 20) == 0 and size >= 64 << 20): cur = "anon(arena-sized)"
        else: cur = "anon(other)"
        counts[cur] += 1
    elif line.startswith("Rss:") and cur:
        groups[cur] += int(line.split()[1])
top = sorted(groups.items(), key=lambda kv: -kv[1])[:8]
for k, v in top: print(f"{v//1024:6d} MiB  {k}  ({counts[k]} maps)")
