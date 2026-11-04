"""Fake Nova helper process for tests; behaviour is steered by arguments."""

import argparse
import json
import os
import sys


def emit(obj):
    # Raw UTF-8 like the real helpers, independent of the locale.
    line = json.dumps(obj, ensure_ascii=False) + "\n"
    sys.stdout.buffer.write(line.encode("utf-8"))
    sys.stdout.buffer.flush()


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--script")
    p.add_argument("--crash-after", type=int)
    p.add_argument("--crash-count-file")
    p.add_argument("--noise", action="store_true")
    p.add_argument("--no-ready", action="store_true")
    p.add_argument("--binary-noise", action="store_true")
    p.add_argument("--fail-with")
    p.add_argument("--log")
    args = p.parse_args()

    sys.stdin.reconfigure(encoding="utf-8")

    on_command = {}
    if args.script:
        with open(args.script, encoding="utf-8") as f:
            on_command = json.load(f).get("auf_befehl", {})

    crash_after = args.crash_after
    fail_with = args.fail_with
    if args.crash_count_file:
        # Crash only on the very first run; --fail-with applies to the later ones.
        if os.path.exists(args.crash_count_file):
            crash_after = None
        else:
            fail_with = None
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
    if not args.no_ready:
        emit({"event": "ready"})

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
