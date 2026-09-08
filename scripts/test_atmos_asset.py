"""Verify compact JOC asset timing and real demuxing across repeated chunk offsets."""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "assets/audio"
spec = importlib.util.spec_from_file_location("extend_atmos", ROOT / "scripts/extend-atmos-assist.py")
extend_atmos = importlib.util.module_from_spec(spec)
spec.loader.exec_module(extend_atmos)


class AtmosAssetTests(unittest.TestCase):
    def test_bundled_asset_is_reproducible_and_manifest_matches(self):
        source = (ASSETS / "atmos-assist-source.m4a").read_bytes()
        asset = (ASSETS / "atmos-assist.m4a").read_bytes()
        self.assertEqual(asset, extend_atmos.extend(source))
        manifest = json.loads((ASSETS / "atmos-assist.json").read_text())
        self.assertEqual(manifest["sha256"], hashlib.sha256(asset).hexdigest())
        self.assertEqual(manifest["bytes"], len(asset))
        self.assertEqual(manifest["encoded_duration_seconds"], 86446.08)
        self.assertLess(len(asset) - len(source), 16 * 1024)
        seed_packets = next(body for _, tag, body in extend_atmos.boxes(source) if tag == b"mdat")
        long_packets = next(body for _, tag, body in extend_atmos.boxes(asset) if tag == b"mdat")
        self.assertEqual(seed_packets, long_packets, "Every compressed JOC byte is preserved")

    def test_rejects_changed_seed_and_duration_overflow(self):
        source = (ASSETS / "atmos-assist-source.m4a").read_bytes()
        with self.assertRaises(ValueError):
            extend_atmos.extend(source[:-1])
        for repeats in (0, -1, extend_atmos.REPEATS + 1):
            with self.assertRaises(ValueError):
                extend_atmos.extend(source, repeats)

    @unittest.skipUnless(shutil.which("ffprobe"), "requires FFmpeg for independent MP4 demuxing")
    def test_demuxes_identical_joc_packets_at_seams_and_at_the_24_hour_tail(self):
        def probe(name, *arguments):
            return json.loads(subprocess.check_output([
                "ffprobe", "-v", "error", *arguments, "-of", "json", str(ASSETS / name)
            ], text=True))

        packet_args = ("-show_packets", "-show_entries", "packet=pts,duration,data_hash",
                       "-show_data_hash", "sha256")
        seed = probe("atmos-assist-source.m4a", *packet_args)["packets"]
        asset = probe("atmos-assist.m4a", "-show_entries", "stream=duration,nb_frames,sample_rate")
        stream = asset["streams"][0]
        self.assertEqual(float(stream["duration"]), 86446.08)
        self.assertEqual(int(stream["nb_frames"]), 2701440)
        self.assertEqual(int(stream["sample_rate"]), 48000)
        intervals = "0%+0.160,29.952%+0.160,59.968%+0.160,86446.016%+0.160"
        packets = probe("atmos-assist.m4a", *packet_args, "-read_intervals", intervals)["packets"]
        self.assertGreaterEqual(len(packets), 15)
        positions = {packet["pts"] for packet in packets}
        for seam in (938 * 1536, 2 * 938 * 1536):
            self.assertTrue({seam - 1536, seam} <= positions)
        self.assertEqual(packets[-1]["pts"] + 1536, 2701440 * 1536)
        for packet in packets:
            index, remainder = divmod(packet["pts"], 1536)
            self.assertEqual(remainder, 0)
            self.assertEqual(packet["duration"], 1536)
            self.assertEqual(packet["data_hash"], seed[index % len(seed)]["data_hash"])


if __name__ == "__main__":
    unittest.main()
