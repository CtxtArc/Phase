#!/usr/bin/env python3
"""Renders docs/tutorial/README.md from docs/tutorial/tutorial.template.md.

Every program block comes from a real file in docs/tutorial/, and every
output block from actually running the command shown, so the tutorial
can't drift from the compiler. CI re-renders it and fails if the result
differs from the committed file.

    {{file NAME}}          the whole program, as a ```phase block
    {{excerpt NAME A-B}}   lines A..B of it
    {{run COMMAND}}        `$ COMMAND` and its real output, as a ```console block

Run from anywhere: python3 scripts/render_tutorial.py
"""

import os
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
TUT = ROOT / "docs" / "tutorial"


def phase_bin():
    subprocess.run(
        ["cargo", "build", "-q", "--bin", "phase"], cwd=ROOT, check=True
    )
    return ROOT / "target" / "debug"


def run(command, bindir):
    env = dict(os.environ)
    env["PATH"] = f"{bindir}{os.pathsep}{env['PATH']}"
    env.pop("PHASE_RUNTIME_DIR", None)
    env.pop("PHASE_STUB_SEED", None)
    env["NO_COLOR"] = "1"
    p = subprocess.run(
        command, shell=True, cwd=TUT, env=env, capture_output=True, text=True
    )
    if command.startswith("phase build") and p.returncode != 0:
        sys.exit(f"render_tutorial: `{command}` failed:\n{p.stderr}")
    out = (p.stdout + p.stderr) if p.returncode == 0 else (p.stderr + p.stdout)
    # Paths on this machine -> a placeholder for the reader's clone.
    out = out.replace(str(ROOT), "/path/to/phase")
    out = "\n".join(line.rstrip() for line in out.rstrip("\n").split("\n"))
    return f"```console\n$ {command}\n{out}\n```"


def render():
    bindir = phase_bin()
    src = (TUT / "tutorial.template.md").read_text()

    def program(name):
        path = TUT / name
        if not path.is_file():
            sys.exit(f"render_tutorial: {name} does not exist")
        return path.read_text().rstrip("\n").split("\n")

    def sub(m):
        kind, arg = m.group(1), m.group(2).strip()
        if kind == "file":
            return "```phase\n" + "\n".join(program(arg)) + "\n```"
        if kind == "excerpt":
            name, span = arg.split()
            a, b = (int(x) for x in span.split("-"))
            lines = program(name)
            if not (1 <= a <= b <= len(lines)):
                sys.exit(f"render_tutorial: {name} has no lines {a}-{b}")
            return f"`{name}`, line{'s' if a != b else ''} {a}{'-' + str(b) if a != b else ''}:\n\n```phase\n" + "\n".join(lines[a - 1 : b]) + "\n```"
        if kind == "run":
            return run(arg, bindir)
        sys.exit(f"render_tutorial: unknown directive {kind}")

    body = re.sub(r"\{\{(file|excerpt|run) ([^}]*)\}\}", sub, src)
    header = (
        "<!-- Generated from tutorial.template.md by scripts/render_tutorial.py;\n"
        "     edit the template or the programs, then re-run the script. -->\n\n"
    )
    (TUT / "README.md").write_text(header + body)


if __name__ == "__main__":
    render()
