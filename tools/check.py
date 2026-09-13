#!/usr/bin/env python3
"""Run fgdb checks, isolating GTK tests and building live-GDB fixtures.

Examples:
    python3 tools/check.py unit
    python3 tools/check.py gtk --filter variables
    python3 tools/check.py gdb
    python3 tools/check.py bench --filter instruction
    python3 tools/check.py clippy --no-default-features

GTK checks require xvfb-run. GDB checks build examples with C, C++, Rust,
C3, GDC, DMD, GNAT, Fortran, Zig and Odin; override the examples/Makefile
compiler variables through the environment as needed. Benchmarks use release
builds and should run on an otherwise idle machine. Each ignored test gets
its own process, including a fresh X server for tests that initialize GTK.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]


def run(command, **kwargs):
    print("+ " + " ".join(map(str, command)), flush=True)
    return subprocess.run(command, cwd=ROOT, check=True, text=True, **kwargs)


def test_binary(features, release):
    command = ["cargo", "test", "--locked", "--no-run", "--message-format=json-render-diagnostics", *features]
    if release:
        command.append("--release")
    output = run(command, stdout=subprocess.PIPE).stdout
    binaries = {
        message["executable"]
        for line in output.splitlines()
        if (message := json.loads(line)).get("reason") == "compiler-artifact"
        and message.get("profile", {}).get("test")
        and message.get("executable")
        and message["target"]["name"] == "fgdb"
    }
    if len(binaries) != 1:
        raise RuntimeError(f"Expected one fgdb test executable, found {binaries}")
    return binaries.pop()


def uses_gtk(name):
    return name.startswith(("ui::", "theme::"))


def group_for(name):
    if name.rsplit("::", 1)[-1].startswith("benchmark_"):
        return "bench"
    return "gtk" if uses_gtk(name) else "gdb"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("group", choices=("unit", "gtk", "gdb", "bench", "clippy"))
    parser.add_argument("--filter", default="", help="Test-name substring")
    parser.add_argument("--list", action="store_true", help="List selected tests without running")
    parser.add_argument("--no-default-features", action="store_true")
    parser.add_argument("--skip-fixtures", action="store_true", help="Use already built GDB fixtures")
    args = parser.parse_args()
    features = ["--no-default-features"] if args.no_default_features else ["--all-features"]

    if args.group == "clippy":
        run(["cargo", "clippy", "--locked", "--all-targets", *features, "--", "-D", "warnings"])
        return

    binary = test_binary(features, args.group == "bench")
    if args.group == "unit":
        run([binary, args.filter, *(["--list"] if args.list else [])])
        return

    listing = run([binary, "--list", "--ignored"], stdout=subprocess.PIPE).stdout
    names = [line.removesuffix(": test") for line in listing.splitlines() if line.endswith(": test")]
    selected = [name for name in names if group_for(name) == args.group and args.filter in name]
    if not selected:
        raise RuntimeError("No tests matched the selected group and filter")
    if args.list:
        print("\n".join(selected))
        return

    if not args.skip_fixtures and any(not uses_gtk(name) for name in selected) and (
        args.group == "gdb" or any(name.startswith("app::") for name in selected)
    ):
        run([
            "make", "-C", "examples", "all", "languages", "allocator-glibc",
            "../target/debug-fixtures/dmd-variable-viewer-target",
            "../target/debug-fixtures/dmd-return-value-target",
        ])

    failed = []
    for name in selected:
        command = [binary, name, "--exact", "--ignored", "--nocapture", "--test-threads=1"]
        environment = dict(os.environ)
        if uses_gtk(name):
            environment.update(GDK_BACKEND="x11", GSK_RENDERER="cairo")
            command = ["xvfb-run", "-a", "-s", "-screen 0 1920x1080x24 -nolisten tcp", *command]
        try:
            run(command, env=environment)
        except subprocess.CalledProcessError:
            failed.append(name)
    print(f"{len(selected) - len(failed)}/{len(selected)} checks passed", flush=True)
    if failed:
        raise RuntimeError("Failed checks:\n" + "\n".join(failed))


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
