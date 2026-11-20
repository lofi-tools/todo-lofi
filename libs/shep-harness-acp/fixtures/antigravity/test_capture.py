import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("capture", Path(__file__).with_name("capture.py"))
capture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(capture)

FAKE = r'''
import json, os, sys
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "session/prompt":
        sys.exit(0)
    result = {}
    if method == "session/new":
        assert os.path.samefile(os.getcwd(), request["params"]["cwd"])
        result = {"sessionId": "fixture"}
    elif method == "session/set_config_option":
        params = request["params"]
        result = {"configOptions": [{"id": params["configId"], "currentValue": params["value"]}]}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
'''


class CaptureTest(unittest.TestCase):
    def test_partial_prompt_is_recorded_and_cannot_be_retried(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.multiple(capture, CACHE=root / "replay", LEGACY_CACHE=root / "legacy", RUNS=root / "runs"):
                with self.assertRaisesRegex(RuntimeError, "closed its output"):
                    capture.capture([sys.executable, "-u", "-c", FAKE])
                events = [json.loads(line) for line in (capture.CACHE / "exchange.jsonl").read_text().splitlines()]
                prompts = [event for event in events if event["message"].get("method") == "session/prompt"]
                self.assertEqual(len(prompts), 1)
                self.assertEqual(len(list((root / "runs").glob("antigravity.*/project/README.md"))), 1)
                self.assertTrue((capture.CACHE / "prompt-sent").exists())
                with self.assertRaisesRegex(RuntimeError, "already attempted"):
                    capture.capture([sys.executable, "-u", "-c", FAKE])

    def test_legacy_marker_blocks_capture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            legacy = root / "legacy"
            legacy.mkdir()
            (legacy / "prompt-sent").touch()
            with patch.multiple(capture, CACHE=root / "replay", LEGACY_CACHE=legacy, RUNS=root / "runs"):
                with self.assertRaisesRegex(RuntimeError, "already attempted"):
                    capture.capture([sys.executable, "-u", "-c", FAKE])
            self.assertFalse((root / "runs").exists())


if __name__ == "__main__":
    unittest.main()
