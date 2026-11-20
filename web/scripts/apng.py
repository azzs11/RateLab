"""Joins PNG frames of the same size into an animated PNG (APNG), with only the
standard library. GitHub READMEs and every current browser play APNG.

    python3 web/scripts/apng.py out.png frame1.png frame2.png ... [--delay-ms 190]
"""

import struct
import sys
import zlib

SIGNATURE = b"\x89PNG\r\n\x1a\n"


def chunks(data):
    if data[:8] != SIGNATURE:
        raise ValueError("not a PNG")
    i = 8
    while i < len(data):
        (length,) = struct.unpack(">I", data[i : i + 4])
        kind = data[i + 4 : i + 8]
        yield kind, data[i + 8 : i + 8 + length]
        i += 12 + length


def chunk(kind, body):
    return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)


def main(argv):
    delay = 190
    if "--delay-ms" in argv:
        k = argv.index("--delay-ms")
        delay = int(argv[k + 1])
        del argv[k : k + 2]
    out, frames = argv[0], argv[1:]
    if not frames:
        raise SystemExit("usage: apng.py out.png frame.png ... [--delay-ms N]")
    parsed = []
    for path in frames:
        with open(path, "rb") as f:
            parsed.append(list(chunks(f.read())))
    ihdr = parsed[0][0][1]
    for p in parsed:
        if p[0][1] != ihdr:
            raise SystemExit("frames differ in size or colour type")
    width, height = struct.unpack(">II", ihdr[:8])
    pre = [c for c in parsed[0] if c[0] not in (b"IHDR", b"IDAT", b"IEND")]
    body = SIGNATURE + chunk(b"IHDR", ihdr)
    body += chunk(b"acTL", struct.pack(">II", len(parsed), 0))  # 0 = loop forever
    for kind, data in pre:
        if kind in (b"PLTE", b"tRNS", b"sRGB", b"gAMA", b"cHRM", b"iCCP", b"pHYs"):
            body += chunk(kind, data)
    seq = 0
    for n, p in enumerate(parsed):
        last = n == len(parsed) - 1
        frame_delay = delay * 8 if last else delay  # rest on the final frame
        body += chunk(b"fcTL", struct.pack(">IIIIIHHBB", seq, width, height, 0, 0, frame_delay, 1000, 0, 0))
        seq += 1
        for kind, data in p:
            if kind != b"IDAT":
                continue
            if n == 0:
                body += chunk(b"IDAT", data)
            else:
                body += chunk(b"fdAT", struct.pack(">I", seq) + data)
                seq += 1
    body += chunk(b"IEND", b"")
    with open(out, "wb") as f:
        f.write(body)
    print(f"wrote {out}: {len(parsed)} frames, {width}x{height}, {len(body) // 1024} KiB")


if __name__ == "__main__":
    main(sys.argv[1:])
