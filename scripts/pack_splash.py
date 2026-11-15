"""Packs splash_*.jpg frames into client-desktop/assets/splash.bin for the start-up splash.

Format: u16 fps, u32 frame count, then per frame u32 length + JPEG bytes (little endian).
Usage: python scripts/pack_splash.py <frames dir> [fps]   (frames named splash_001.jpg, splash_002.jpg, ...)
"""
import glob, os, struct, sys

src = sys.argv[1]
fps = int(sys.argv[2]) if len(sys.argv) > 2 else 20
frames = sorted(glob.glob(os.path.join(src, "splash_*.jpg")))
if not frames:
    sys.exit("no splash_*.jpg in " + src)
out = os.path.join(os.path.dirname(__file__), "..", "client-desktop", "assets", "splash.bin")
os.makedirs(os.path.dirname(out), exist_ok=True)
with open(out, "wb") as f:
    f.write(struct.pack("<HI", fps, len(frames)))
    for p in frames:
        data = open(p, "rb").read()
        f.write(struct.pack("<I", len(data)) + data)
print(len(frames), "frames,", os.path.getsize(out), "bytes ->", os.path.normpath(out))
