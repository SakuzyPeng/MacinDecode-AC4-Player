#!/usr/bin/env python3
"""Extend the checked-in JOC seed without duplicating or re-encoding its packets.

Each MP4 chunk references the same mdat payload. The sample timeline is continuous,
so AVPlayer keeps its item, decoder and silent tap across the 30-second seed seams.
This is an offline asset-generation step, never a build or playback dependency.
"""
import argparse
import hashlib
from pathlib import Path
import struct

SOURCE_SHA256 = "b4a655f657abecc1568c5e03ff6607fe5d28e19474d99adf04e13aed9904f9d0"
REPEATS = 2880  # 24 h 46.08 s; all version-0 MP4 durations still fit uint32.
SEED_PACKETS = 938
PACKET_BYTES = 1536
PACKET_FRAMES = 1536
CONTAINERS = {b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts"}


def boxes(data):
    offset = 0
    while offset < len(data):
        length, tag = struct.unpack_from(">I4s", data, offset)
        if length < 8 or offset + length > len(data):
            raise ValueError("Invalid MP4 box")
        yield offset, tag, data[offset + 8:offset + length]
        offset += length


def box(tag, body):
    return struct.pack(">I4s", len(body) + 8, tag) + body


def extend(source, repeats=REPEATS):
    # This deliberately handles one known seed, not arbitrary media or MP4
    # layouts. A replacement seed needs its timing/sample-table contract reviewed.
    if hashlib.sha256(source).hexdigest() != SOURCE_SHA256:
        raise ValueError("Expected the original 30.016-second atmos-assist-source.m4a")
    if not 1 <= repeats <= REPEATS:
        raise ValueError(f"Repeat count must be in 1..{REPEATS}")
    packets = SEED_PACKETS * repeats

    def rewrite(data, chunk_offset):
        result = bytearray()
        for _, tag, payload in boxes(data):
            body = bytearray(payload)
            if tag in CONTAINERS:
                body = rewrite(body, chunk_offset)
            elif tag in (b"mvhd", b"mdhd", b"tkhd", b"elst"):
                duration_offset = {b"mvhd": 16, b"mdhd": 16, b"tkhd": 20, b"elst": 8}[tag]
                duration, = struct.unpack_from(">I", body, duration_offset)
                struct.pack_into(">I", body, duration_offset, duration * repeats)
            elif tag == b"stts":
                body = struct.pack(">4I", 0, 1, packets, PACKET_FRAMES)
            elif tag == b"stsz":
                body = struct.pack(">3I", 0, PACKET_BYTES, packets)
            elif tag == b"stsc":
                body = struct.pack(">5I", 0, 1, 1, SEED_PACKETS, 1)
            elif tag == b"stco":
                body = struct.pack(">2I", 0, repeats) + struct.pack(">I", chunk_offset) * repeats
            result += box(tag, body)
        return bytes(result)

    # Expanding moov moves mdat. Resolve the absolute chunk address only after
    # every table has its final size; no source offsets survive into the output.
    sized = rewrite(source, 0)
    chunk_offset = next(offset + 8 for offset, tag, _ in boxes(sized) if tag == b"mdat")
    return rewrite(source, chunk_offset)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    if args.source.resolve() == args.output.resolve():
        parser.error("Keep the original seed: source and output must differ")
    data = extend(args.source.read_bytes())
    args.output.write_bytes(data)
    print(f"{args.output}: {len(data)} bytes, SHA-256 {hashlib.sha256(data).hexdigest()}")


if __name__ == "__main__":
    main()
