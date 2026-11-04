"""Fake Nova helper process for tests; behaviour is steered by arguments."""

import argparse
import json
import os
import subprocess
import sys
import threading
import time


def emit(obj):
    # Raw UTF-8 like the real helpers, independent of the locale.
    line = json.dumps(obj, ensure_ascii=False) + "\n"
    sys.stdout.buffer.write(line.encode("utf-8"))
    sys.stdout.buffer.flush()


def exit_once_present(path):
    while not os.path.exists(path):
        time.sleep(0.01)
    os._exit(1)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--script")
    p.add_argument("--crash-after", type=int)
    p.add_argument("--crash-count-file")
    p.add_argument("--noise", action="store_true")
    p.add_argument("--no-ready", action="store_true")
    p.add_argument("--binary-noise", action="store_true")
    p.add_argument("--fail-with")
    p.add_argument("--ready-gate")
    p.add_argument("--exit-gate")
    p.add_argument("--stubborn")
    p.add_argument("--log")
    args = p.parse_args()

    sys.stdin.reconfigure(encoding="utf-8")

    on_command = {}
    if args.script:
        with open(args.script, encoding="utf-8") as f:
            on_command = json.load(f).get("auf_befehl", {})

    crash_after = args.crash_after
    fail_with = args.fail_with
    ready_gate = args.ready_gate
    exit_gate = args.exit_gate
    if args.crash_count_file:
        # Crash only on the very first run; --fail-with and --ready-gate apply to the later ones.
        if os.path.exists(args.crash_count_file):
            crash_after = None
            exit_gate = None
        else:
            fail_with = None
            ready_gate = None
            with open(args.crash_count_file, "w", encoding="utf-8") as f:
                f.write("1")

    if fail_with:
        emit({"event": "error", "message": fail_with})
        sys.exit(1)

    if args.noise:
        print("kein json", flush=True)
    if args.binary_noise:
        # Invalid UTF-8 on both streams, before anything else.
        sys.stdout.buffer.write(b"\xff\xfe kein utf8\n")
        sys.stdout.buffer.flush()
        sys.stderr.buffer.write(b"\xff\xfe kein utf8\n")
        sys.stderr.buffer.flush()
    if ready_gate:
        if args.log:
            with open(args.log, "a", encoding="utf-8") as f:
                f.write("gate\n")
        while not os.path.exists(ready_gate):
            time.sleep(0.01)
    if args.stubborn:
        # Writes "<own pid> <child pid>" and will not react to a closed stdin.
        child = subprocess.Popen(
            ["sleep", "60"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        with open(args.stubborn, "w", encoding="utf-8") as f:
            f.write(f"{os.getpid()} {child.pid}")
    if not args.no_ready:
        emit({"event": "ready"})
    if exit_gate:
        threading.Thread(target=exit_once_present, args=(exit_gate,), daemon=True).start()
    if args.stubborn:
        time.sleep(60)

    count = 0
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        if args.log:
            with open(args.log, "a", encoding="utf-8") as f:
                f.write(line + "\n")
        count += 1
        try:
            cmd = json.loads(line).get("cmd")
        except (ValueError, AttributeError):
            cmd = None
        for event in on_command.get(cmd, []):
            emit(event)
        if crash_after is not None and count >= crash_after:
            sys.exit(1)


if __name__ == "__main__":
    main()
