import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class MaintenanceCLI(unittest.TestCase):
    def test_maintenance_never_starts_receiver_and_keeps_paths_as_single_arguments(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            adb = directory / "adb"
            adb.write_text("#!/usr/bin/env python3\nimport json, os, sys\nwith open(os.environ['SPOTIYA_TEST_LOG'], 'a') as f: f.write(json.dumps(sys.argv[1:]) + '\\n')\n")
            adb.chmod(0o700)
            log = directory / "calls.jsonl"
            env = dict(os.environ, PATH=str(directory) + os.pathsep + os.environ["PATH"], SPOTIYA_TEST_LOG=str(log))
            path = "/tmp/report with spaces; $(false).json"
            cases = [
                (["addon", "install", "airplay"], ["--addon-install", "airplay"]),
                (["addon", "remove", "airplay"], ["--addon-remove", "airplay"]),
                (["addon", "file", "airplay", path, "/tmp/AIRPLAY_SHA256SUMS"], ["--addon-install-file", "airplay", path, "/tmp/AIRPLAY_SHA256SUMS"]),
                (["doctor"], ["--doctor"]),
                (["doctor", "report", path], ["--doctor-report", path]),
                (["update", "check"], ["--update-check"]),
                (["update", "v0.8.0"], ["--update-tag", "v0.8.0"]),
                (["update", "file", path, "/tmp/SHA256SUMS"], ["--update-file", path, "/tmp/SHA256SUMS"]),
            ]
            for command, expected in cases:
                result = subprocess.run([str(ROOT / "bin/spotiya"), "--device", "fixture:5555"] + command, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(len(calls), len(cases))
            for call, (_, expected) in zip(calls, cases):
                self.assertEqual(call[:3], ["-s", "fixture:5555", "shell"])
                self.assertEqual(shlex.split(call[3]), ["/data/opt/spotify-connect/spotiya"] + expected)
