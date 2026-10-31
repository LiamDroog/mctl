#!/usr/bin/env python3
"""Convert a Natural Earth PolyLine shapefile to mctl's compact coastline format.

Format (little endian): u32 line_count, then per line: u32 point_count followed by
point_count * (f32 lon_deg, f32 lat_deg). Only parts of PolyLine records are kept
as separate lines. Usage: shp_to_coast.py in.shp out.bin
"""
import struct
import sys

SHAPE_POLYLINE = 3


def read_lines(path):
    data = open(path, "rb").read()
    if len(data) < 100:
        raise SystemExit(f"{path}: too short for a shapefile header")
    code, = struct.unpack(">i", data[0:4])
    if code != 9994:
        raise SystemExit(f"{path}: bad shapefile magic {code}")
    off = 100
    lines = []
    while off + 8 <= len(data):
        _num, words = struct.unpack(">ii", data[off:off + 8])
        rec = data[off + 8: off + 8 + words * 2]
        off += 8 + words * 2
        stype, = struct.unpack("<i", rec[0:4])
        if stype == 0:
            continue
        if stype != SHAPE_POLYLINE:
            raise SystemExit(f"{path}: unexpected shape type {stype}")
        nparts, npts = struct.unpack("<ii", rec[36:44])
        parts = struct.unpack(f"<{nparts}i", rec[44:44 + 4 * nparts])
        pts_off = 44 + 4 * nparts
        pts = [struct.unpack("<dd", rec[pts_off + 16 * i: pts_off + 16 * i + 16]) for i in range(npts)]
        bounds = list(parts) + [npts]
        for a, b in zip(bounds, bounds[1:]):
            if b - a >= 2:
                lines.append(pts[a:b])
    if not lines:
        raise SystemExit(f"{path}: no polylines found")
    return lines


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    lines = read_lines(sys.argv[1])
    out = bytearray(struct.pack("<I", len(lines)))
    for ln in lines:
        out += struct.pack("<I", len(ln))
        for lon, lat in ln:
            out += struct.pack("<ff", lon, lat)
    open(sys.argv[2], "wb").write(out)
    print(f"{len(lines)} lines, {sum(len(l) for l in lines)} points, {len(out)} bytes")


main()
