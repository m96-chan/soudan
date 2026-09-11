#!/usr/bin/env python3
"""Opt-in Kitty desktop smoke test. Uses a temporary chat fixture, not an LLM."""
import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import time

binary = str(pathlib.Path("target/debug/soudan").resolve())
with tempfile.TemporaryDirectory(prefix="soudan-terminal-") as directory:
    root = pathlib.Path(directory)
    executable = root / "cursor-agent" / "node"
    executable.parent.mkdir()
    shutil.copyfile(shutil.which("python3"), executable)
    executable.chmod(0o700)
    chat = root / "chat.py"
    chat.write_text("while True:\n text=input('❯ ')\n print('Fixture reply: '+text, flush=True)\n")
    address = "unix:" + str(root / "kitty-control")
    terminal = subprocess.Popen(
        ["kitty", "--config", "NONE", "--override", "allow_remote_control=socket-only",
         "--listen-on", address, "--start-as", "hidden", "--directory", directory,
         str(executable), str(chat)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
    )
    def run(*args):
        output = subprocess.run([binary, "--workspace", directory, *args],
                                capture_output=True, text=True, timeout=25)
        if output.returncode:
            raise RuntimeError(output.stderr)
        return json.loads(output.stdout)
    try:
        for _ in range(50):
            if (root / "kitty-control").exists():
                break
            if terminal.poll() is not None:
                raise RuntimeError(terminal.stderr.read().decode())
            time.sleep(0.1)
        subprocess.run(["kitty", "@", "--to", address, "launch", "--type", "background",
                        "--allow-remote-control", "--remote-control-password", "!",
                        "--remote-control-password", '"" get-text send-text send-key',
                        "--cwd", directory, binary, "--workspace", directory, "live", "bridge"],
                       check=True, capture_output=True, timeout=10)
        for _ in range(50):
            if (root / ".soudan/kitty.sock").exists():
                break
            time.sleep(0.1)
        targets = run("live", "list")
        assert len(targets) == 1, targets
        target = targets[0]["id"]
        assert "❯" in run("live", "read", target)["screen"]
        first = run("live", "send", target, "--request-id", "fixture-1", "Hello existing terminal")
        assert first["status"] == "submitted", first
        time.sleep(0.5)
        after = run("live", "read", target)["screen"]
        assert "Fixture reply:" in after and "Hello existing terminal" in after, after
        second = run("live", "send", target, "--request-id", "fixture-1", "Hello existing terminal")
        assert second["replayed"] is True, second
        assert run("live", "delivery", "fixture-1")["status"] == "submitted"
        assert run("live", "read", target)["screen"].count("Fixture reply:") == 1
        subprocess.run(["kitty", "@", "--to", address, "send-text", "--match",
                        f"id:{targets[0]['window_id']}", "--stdin"], input="unfinished draft",
                       text=True, check=True, capture_output=True, timeout=10)
        time.sleep(0.2)
        rejected = subprocess.run([binary, "--workspace", directory, "live", "send", target,
                                   "--request-id", "fixture-2", "Do not append this"],
                                  capture_output=True, text=True, timeout=25)
        assert rejected.returncode != 0 and "draft" in rejected.stderr, rejected.stderr
        assert "Do not append this" not in run("live", "read", target)["screen"]
        assert run("live", "disconnect")["status"] == "disconnected"
        print("PASS: real Kitty delivery, visible reply, retry deduplication, draft protection, and disconnect.")
    finally:
        terminal.terminate()
        try:
            terminal.wait(timeout=5)
        except subprocess.TimeoutExpired:
            terminal.kill()
            terminal.wait()
