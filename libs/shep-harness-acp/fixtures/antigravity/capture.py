#!/usr/bin/env python3
"""Capture the one authorized Antigravity ACP prompt in a disposable project."""

import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[4]
CACHE = ROOT / ".cache/shepherd-live/antigravity"
PROMPT = "Read README.md and ../outside.txt, run pwd and ls, then briefly report."


def record(stream, direction, message):
    stream.write(json.dumps({"direction": direction, "message": message}) + "\n")
    stream.flush()
    os.fsync(stream.fileno())


def capture(command):
    CACHE.mkdir(parents=True, exist_ok=True)
    marker = CACHE / "prompt-sent"
    if marker.exists():
        raise RuntimeError(f"A prompt was already attempted; inspect {CACHE} and do not retry")
    project = CACHE / "fixture/project"
    project.mkdir(parents=True, exist_ok=True)
    (project / "README.md").write_text("Shepherd ACP capture fixture.\n")
    (project.parent / "outside.txt").write_text("Neighboring ACP capture fixture.\n")
    process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, text=True, bufsize=1)
    messages = queue.Queue()

    def read_output():
        try:
            for line in process.stdout:
                messages.put(json.loads(line))
        except Exception as error:
            messages.put(error)
        finally:
            messages.put(None)

    reader = threading.Thread(target=read_output, daemon=True)
    reader.start()
    try:
        with (CACHE / "exchange.jsonl").open("x") as transcript:
            def send(message):
                record(transcript, "client", message)
                process.stdin.write(json.dumps(message) + "\n")
                process.stdin.flush()

            def receive_until(request_id, timeout):
                deadline = time.monotonic() + timeout
                while True:
                    message = messages.get(timeout=max(0.01, deadline - time.monotonic()))
                    if message is None:
                        raise RuntimeError("ACP server closed its output")
                    if isinstance(message, Exception):
                        raise message
                    record(transcript, "agent", message)
                    if message.get("id") == request_id and "method" not in message:
                        if "error" in message:
                            raise RuntimeError(f"ACP request failed: {message['error']}")
                        return message.get("result", {})
                    if "method" in message and "id" in message:
                        if message["method"] == "session/request_permission":
                            send({"jsonrpc": "2.0", "id": message["id"], "result": {
                                "outcome": {"outcome": "cancelled"}}})
                        else:
                            send({"jsonrpc": "2.0", "id": message["id"], "error": {
                                "code": -32601, "message": "Unavailable in capture fixture"}})

            def request(request_id, method, params, timeout=30):
                send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
                return receive_until(request_id, timeout)

            request(1, "initialize", {"protocolVersion": 1, "clientInfo": {
                "name": "shepherd-capture", "version": "1"}, "clientCapabilities": {}})
            session = request(2, "session/new", {"cwd": str(project), "mcpServers": []})["sessionId"]
            for request_id, name, value in [(3, "mode", "auto"), (4, "model", "gemini-3.8-flash")]:
                result = request(request_id, "session/set_config_option", {
                    "sessionId": session, "configId": name, "value": value})
                if not any(option.get("id") == name and option.get("currentValue") == value
                           for option in result.get("configOptions", [])):
                    raise RuntimeError(f"Server did not confirm {name}={value}")
            with marker.open("x") as file:
                file.write(PROMPT + "\n")
                file.flush()
                os.fsync(file.fileno())
            request(5, "session/prompt", {"sessionId": session, "prompt": [
                {"type": "text", "text": PROMPT}]}, timeout=240)
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        reader.join(timeout=5)
        process.stdin.close()
        process.stdout.close()


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit("usage: capture.py /absolute/path/to/agy acp")
    capture(sys.argv[1:])
