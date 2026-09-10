#!/usr/bin/env python3
"""Opt-in real-provider smoke test; consumes the configured CLI accounts' usage."""
import argparse
import json
import pathlib
import subprocess
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="soudan")
    parser.add_argument("--workspace", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--agents", nargs="+", default=["claude-code", "cursor", "codex"])
    args = parser.parse_args()
    room = "smoke-" + str(uuid.uuid4())
    command = [args.binary, "--workspace", str(args.workspace.resolve())]
    for index, agent in enumerate(args.agents):
        prompt = (
            "Propose one acceptance test for reliable dialogue between AI agents."
            if index == 0
            else "Critique the preceding agent's acceptance test and refine it."
        ) + " Respond in English in under 80 words. Do not use tools or modify files."
        print(f"Consulting {agent} in {room}...", flush=True)
        result = subprocess.run(
            command + ["consult", "--agent", agent, "--room", room, "--timeout",
                       str(args.timeout), "--wait", prompt],
            check=True, capture_output=True, text=True, timeout=args.timeout + 20,
        )
        job = json.loads(result.stdout)
        if job["status"] != "completed" or not job.get("result"):
            raise RuntimeError(f"Consultation failed: {job}")
        print(job["result"], flush=True)
    history = subprocess.run(command + ["history", "--room", room], check=True,
                             capture_output=True, text=True, timeout=10)
    messages = json.loads(history.stdout)
    responders = [message["sender"] for message in messages if message["sender"] != "requester"]
    if responders != args.agents:
        raise RuntimeError(f"Unexpected responders: {responders}")
    print(f"PASS: {len(args.agents)} real agents replied in room {room}.")


if __name__ == "__main__":
    main()
