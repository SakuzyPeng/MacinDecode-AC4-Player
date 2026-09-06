#!/usr/bin/env python3
"""Generate our own 30 s, 48 kHz moving-object ADM source; no third-party media.

Encode the resulting RF64 once with an E-AC-3/JOC encoder at 384 kbps, then
losslessly mux the EC3 into M4A. The encoder is not a build/runtime dependency.
"""
import argparse
import math
import pathlib
import struct
import xml.etree.ElementTree as ET


def generate(destination):
    rate, seconds = 48000, 30
    root = ET.Element("ebuCoreMain", {"xmlns": "urn:ebu:metadata-schema:ebuCore_2017"})
    afe = ET.SubElement(ET.SubElement(ET.SubElement(root, "coreMetadata"), "format"),
                        "audioFormatExtended", version="ITU-R_BS.2076-2")
    def add(parent, name, text=None, **attributes):
        node = ET.SubElement(parent, name, attributes)
        node.text = text
        return node
    programme = add(afe, "audioProgramme", audioProgrammeID="APR_1001",
                    audioProgrammeName="MacinDecode synthetic Atmos assist", start="00:00:00.00000", end="00:00:30.00000")
    add(programme, "audioContentIDRef", "ACO_1001")
    content = add(afe, "audioContent", audioContentID="ACO_1001", audioContentName="Generated signal")
    add(content, "audioObjectIDRef", "AO_1001")
    obj = add(afe, "audioObject", audioObjectID="AO_1001", audioObjectName="Moving sine",
              start="00:00:00.00000", duration="00:00:30.00000")
    add(obj, "audioPackFormatIDRef", "AP_00031001")
    add(obj, "audioTrackUIDRef", "ATU_00000001")
    pack = add(afe, "audioPackFormat", audioPackFormatID="AP_00031001", audioPackFormatName="Object pack", typeLabel="0003", typeDefinition="Objects")
    add(pack, "audioChannelFormatIDRef", "AC_00031001")
    channel = add(afe, "audioChannelFormat", audioChannelFormatID="AC_00031001", audioChannelFormatName="Moving object", typeLabel="0003", typeDefinition="Objects")
    for i in range(seconds * 25):
        t = i / 25
        block = add(channel, "audioBlockFormat", audioBlockFormatID=f"AB_00031001_{i+1:08x}",
                    rtime=f"00:00:{t:08.5f}", duration="00:00:00.04000")
        add(block, "cartesian", "1")
        angle = 2 * math.pi * t / seconds
        for coordinate, value in [("X", 0.7 * math.sin(angle)), ("Y", 0.7 * math.cos(angle)), ("Z", 0.3 + 0.2 * math.sin(angle))]:
            add(block, "position", f"{value:.7f}", coordinate=coordinate)
        add(block, "gain", "1.0")
    stream = add(afe, "audioStreamFormat", audioStreamFormatID="AS_00031001", audioStreamFormatName="PCM stream", formatLabel="0001", formatDefinition="PCM")
    add(stream, "audioChannelFormatIDRef", "AC_00031001")
    add(stream, "audioTrackFormatIDRef", "AT_00031001_01")
    track = add(afe, "audioTrackFormat", audioTrackFormatID="AT_00031001_01", audioTrackFormatName="PCM track", formatLabel="0001", formatDefinition="PCM")
    add(track, "audioStreamFormatIDRef", "AS_00031001")
    uid = add(afe, "audioTrackUID", UID="ATU_00000001", sampleRate=str(rate), bitDepth="24")
    add(uid, "audioTrackFormatIDRef", "AT_00031001_01")
    add(uid, "audioPackFormatIDRef", "AP_00031001")
    pcm = bytearray(rate * seconds * 3)
    for i in range(rate * seconds):
        fade = min(1, i / 960, (rate * seconds - 1 - i) / 960)
        sample = round((2**23 - 1) * 10**(-30/20) * fade * math.sin(2 * math.pi * 997 * i / rate))
        pcm[i*3:i*3+3] = (sample & 0xffffff).to_bytes(3, "little")
    def chunk(name, data):
        return name + struct.pack("<I", len(data)) + data + b"\0" * (len(data) % 2)
    chna = struct.pack("<HHH12s14s11sx", 1, 1, 1, b"ATU_00000001", b"AT_00031001_01", b"AP_00031001")
    payload = chunk(b"fmt ", struct.pack("<HHIIHH", 1, 1, rate, rate*3, 3, 24))
    payload += chunk(b"bext", b"MacinDecode generated calibration signal".ljust(602, b"\0"))
    payload += chunk(b"axml", ET.tostring(root, encoding="utf-8", xml_declaration=True))
    payload += chunk(b"chna", chna) + chunk(b"data", pcm)
    ds64 = chunk(b"ds64", struct.pack("<QQQI", 4 + 36 + len(payload), len(pcm), rate*seconds, 0))
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open("xb") as output:
        output.write(b"RF64\xff\xff\xff\xffWAVE" + ds64 + payload)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=pathlib.Path)
    generate(parser.parse_args().output)
