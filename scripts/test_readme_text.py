"""Regression checks for diagram text ownership and browser discovery.

Node runs the actual DOM probe against measured text fixtures; no browser or
display is needed. Font measurement itself remains the browser's job.
"""

import html
import importlib.util
import json
import os
from contextlib import chdir
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch


SPEC = importlib.util.spec_from_file_location(
    "readme_text", Path(__file__).with_name("check-readme-text.py"))
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)

NODE = shutil.which("node")
PROBE_HARNESS = """
const vm = require('node:vm');
const input = JSON.parse(require('node:fs').readFileSync(0, 'utf8'));
const scalar = value => ({baseVal: {value}});
const position = value => ({baseVal: {getItem: () => ({value})}});
const panels = input.panels.map(p => ({
  x: scalar(p.x), y: scalar(p.y), width: scalar(p.w), height: scalar(p.h)
}));
const texts = input.texts.map(t => ({
  textContent: t.text, x: position(t.x), y: position(t.y),
  getBBox: () => ({x: t.left, y: t.y - 12, width: t.width, height: 12}),
  style: {fontFamily: 'sans-serif', textAnchor: t.anchor}
}));
const svg = {
  width: scalar(880),
  querySelectorAll: selector => selector === 'rect' ? panels : texts
};
const output = {textContent: ''};
vm.runInNewContext(input.probe, {
  document: {querySelector: () => svg, getElementById: () => output},
  getComputedStyle: text => text.style
}, {timeout: 2000});
process.stdout.write(output.textContent);
"""


@unittest.skipUnless(NODE, "Node.js is required to exercise the SVG DOM probe")
class PanelOwnershipTests(unittest.TestCase):
    def probe(self, texts, panels):
        # Windows CI has exceeded the old 10-second process limit. The VM's
        # separate limit still catches a probe that loops indefinitely.
        result = subprocess.run(
            [NODE, "-e", PROBE_HARNESS],
            input=json.dumps({"probe": checker.PROBE, "texts": texts, "panels": panels}),
            capture_output=True, text=True, encoding="utf-8", check=True, timeout=60)
        return json.loads(result.stdout)

    def test_widening_text_does_not_detach_it_from_its_panel(self):
        # The endpoint box in playback-paths.svg is 136 px wide. Previously
        # this 145 px centred label escaped to the 880 px card and passed,
        # despite extending 4.5 px past BOTH edges of its own box.
        panels = [{"x": 720, "y": 107, "w": 136, "h": 46}]
        for anchor, x, left in (("middle", 788, 715.5), ("end", 842, 697),
                                ("start", 734, 734)):
            with self.subTest(anchor=anchor):
                text = {"text": "Wider endpoint label", "x": x, "y": 130,
                        "left": left, "width": 145, "anchor": anchor}
                run, = self.probe([text], panels)
                self.assertEqual(run[3:5], [720, 856])
                self.assertEqual(run[6], "panel")
                self.assertTrue(run[1] < run[3] or run[1] + run[2] > run[4])

    def test_overflow_into_a_neighbour_keeps_the_original_panel(self):
        panels = [{"x": 450, "y": 62, "w": 406, "h": 300},
                  {"x": 24, "y": 62, "w": 406, "h": 300}]
        text = {"text": "Right panel label", "x": 540, "y": 90,
                "left": 420, "width": 240, "anchor": "middle"}
        run, = self.probe([text], panels)
        self.assertEqual(run[3:5], [450, 856])

    def test_nested_panels_and_card_captions_keep_their_bounds(self):
        panels = [{"x": 24, "y": 62, "w": 832, "h": 300},
                  {"x": 40, "y": 80, "w": 260, "h": 100}]
        texts = [{"text": "Nested", "x": 60, "y": 110,
                  "left": 60, "width": 60, "anchor": "start"},
                 {"text": "Footer", "x": 858, "y": 440,
                  "left": 798, "width": 60, "anchor": "end"}]
        inner, footer = self.probe(texts, panels)
        self.assertEqual(inner[3:5], [40, 300])
        self.assertEqual(footer[3:5], [0, 880])
        self.assertEqual(footer[6], "card")


class BrowserDiscoveryTests(unittest.TestCase):
    def discover(self, platform, installed, environment=None):
        # Windows resolves Path.home() through environment variables that this
        # fixture clears. Keep the home directory independent of the fake OS.
        fixture_home = Path.home()
        with patch.object(checker.sys, "platform", platform), \
                patch.object(Path, "home", return_value=fixture_home), \
                patch.dict(os.environ, environment or {}, clear=True), \
                patch.object(checker.shutil, "which", side_effect=lambda p: p if p == installed else None), \
                patch.object(checker.subprocess, "run", side_effect=AssertionError("no external which")):
            return checker.chrome()

    def test_browser_on_path(self):
        with patch.object(checker.shutil, "which", return_value="/custom/bin/chromium"):
            self.assertEqual(checker.chrome(), "/custom/bin/chromium")

    def test_macos_application_bundles(self):
        for root in (Path("/Applications"), Path.home() / "Applications"):
            for name in ("Google Chrome", "Chromium", "Microsoft Edge"):
                with self.subTest(root=root, browser=name):
                    executable = str(root / f"{name}.app/Contents/MacOS/{name}")
                    self.assertEqual(self.discover("darwin", executable), executable)

    def test_windows_user_and_system_installs_without_unix_which(self):
        for key in ("LOCALAPPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)"):
            for relative in ("Google/Chrome/Application/chrome.exe",
                             "Chromium/Application/chrome.exe",
                             "Microsoft/Edge/Application/msedge.exe"):
                with self.subTest(location=key, browser=relative):
                    root = Path(tempfile.gettempdir()) / "Browser Programs"
                    executable = str(root / relative)
                    self.assertEqual(self.discover("win32", executable, {key: str(root)}), executable)

    def test_bundled_linux_chromium(self):
        executable = "/opt/pw-browsers/chromium-1194/chrome-linux/chrome"
        self.assertEqual(self.discover("linux", executable), executable)

    def test_explicit_executable_with_spaces(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "Custom Browser.exe"
            executable.touch()
            executable.chmod(0o755)
            self.assertEqual(checker.chrome(str(executable)), str(executable))
            with chdir(directory):
                resolved = checker.chrome("./Custom Browser.exe")
                self.assertEqual(Path(resolved).resolve(), executable.resolve())

    def test_invalid_override_does_not_silently_select_another_browser(self):
        with patch.object(checker.shutil, "which", side_effect=lambda p: None if p == "missing-browser" else p):
            with self.assertRaisesRegex(SystemExit, "missing-browser"):
                checker.chrome("missing-browser")

    def test_missing_browser_has_actionable_error_without_spawning_which(self):
        with patch.object(checker.shutil, "which", return_value=None), \
                patch.object(checker.subprocess, "run", side_effect=AssertionError("no external which")):
            with self.assertRaisesRegex(SystemExit, "--browser PATH"):
                checker.chrome()


class BrowserInvocationTests(unittest.TestCase):
    def test_page_url_is_absolute_and_encoded_and_output_is_utf8(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory) / "diagrams #读数"
            scratch.mkdir()
            svg = scratch / "reading.svg"
            svg.write_text('<svg xmlns="http://www.w3.org/2000/svg"/>', encoding="utf-8")
            expected = [["−∞ & gain", 42, 100, 24, 284, "start", "panel"]]
            dom = '<pre id="o">' + html.escape(json.dumps(expected, ensure_ascii=False)) + '</pre>'
            browser = str(Path(directory) / "Custom Browser.exe")
            with patch.object(checker.subprocess, "run", return_value=Mock(stdout=dom)) as run:
                self.assertEqual(checker.measure(svg, scratch, browser), expected)
            command = run.call_args.args[0]
            self.assertEqual(command[0], browser)
            self.assertEqual(command[-1], (scratch / "reading-text.html").resolve().as_uri())
            self.assertNotIn(" ", command[-1])
            self.assertIn("%23", command[-1])
            self.assertEqual(run.call_args.kwargs["encoding"], "utf-8")


if __name__ == "__main__":
    unittest.main()
