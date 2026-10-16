#!/usr/bin/env python3
"""Checks the PHASE TextMate grammar against real source.

A minimal TextMate tokenizer -- `match` rules, `begin`/`end` rules,
`include`s, captures; at each position the earliest match wins, ties going
to the first rule -- is enough for this grammar, whose patterns are all
plain regular expressions Python's `re` understands the same way
Oniguruma does. Then:

1. every example and bug-gallery file tokenizes, and every keyword the
   lexer knows gets a keyword-ish scope wherever it appears as a word;
2. a handful of lines get exactly the scopes we mean them to.

Run from anywhere: python3 editors/vscode/test/grammar_check.py
"""

import json
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]
GRAMMAR = json.loads((HERE.parent / "syntaxes" / "phase.tmLanguage.json").read_text())


def compile_rules(patterns):
    rules = []
    for p in patterns:
        if "include" in p:
            target = GRAMMAR["repository"][p["include"][1:]]
            if "begin" in target or "match" in target:
                rules.extend(compile_rules([target]))
            else:
                rules.extend(compile_rules(target["patterns"]))
        elif "match" in p:
            rules.append(("match", re.compile(p["match"]), p))
        elif "begin" in p:
            rules.append(("begin", re.compile(p["begin"]), p))
    return rules


ROOT_RULES = compile_rules(GRAMMAR["patterns"])
_cache = {}


def rules_of(rule):
    key = id(rule)
    if key not in _cache:
        _cache[key] = compile_rules(rule.get("patterns", []))
    return _cache[key]


def tokenize(text):
    """[(line_no, start, end, [scopes])] for every scoped piece of `text`.
    A named begin/end region is reported line by line, over its whole
    extent (an unterminated one runs to the end of the text)."""
    out = []
    stack = []  # [(rule, compiled end regex, start line, start col)]
    for n, line in enumerate(text.split("\n"), 1):
        pos = 0
        while pos <= len(line):
            rules = rules_of(stack[-1][0]) if stack else ROOT_RULES
            outer = [r[0].get("name") for r in stack if r[0].get("name")]
            best = None
            if stack:
                m = stack[-1][1].search(line, pos)
                if m:
                    best = (m.start(), -1, "end", m, stack[-1][0])
            for i, (kind, rx, rule) in enumerate(rules):
                m = rx.search(line, pos)
                if m and (best is None or (m.start(), i) < best[:2]):
                    best = (m.start(), i, kind, m, rule)
            if best is None:
                break
            _, _, kind, m, rule = best
            if kind == "end":
                _, _, sl, sc = stack.pop()
                if rule.get("name"):
                    region(out, text, sl, sc, n, m.end(), outer + [rule["name"]])
                caps = rule.get("endCaptures", {})
            else:
                caps = rule.get("captures", rule.get("beginCaptures", {}))
                name = rule.get("name")
                if name and kind == "match":
                    out.append((n, m.start(), m.end(), outer + [name]))
                if kind == "begin":
                    stack.append((rule, re.compile(rule["end"]), n, m.start()))
            for g, c in caps.items():
                if m.group(int(g)) is not None:
                    out.append((n, m.start(int(g)), m.end(int(g)), outer + [c["name"]]))
            # Never loop on an empty match.
            pos = m.end() if m.end() > pos else pos + 1
    lines = text.split("\n")
    while stack:
        rule, _, sl, sc = stack.pop()
        outer = [r[0].get("name") for r in stack if r[0].get("name")]
        if rule.get("name"):
            region(out, text, sl, sc, len(lines), len(lines[-1]), outer + [rule["name"]])
    return out


def region(out, text, sl, sc, el, ec, scopes):
    """A begin/end rule's scopes over (sl, sc) .. (el, ec), line by line."""
    lines = text.split("\n")
    for n in range(sl, el + 1):
        start = sc if n == sl else 0
        end = ec if n == el else len(lines[n - 1])
        out.append((n, start, end, scopes))


def scopes_at(tokens, line_no, col, line):
    """Scopes covering `line[col]` on `line_no`."""
    found = []
    for n, s, e, scopes in tokens:
        if n == line_no and s <= col < e:
            found.extend(scopes)
    return found


KEYWORDS = (
    "import entity fn let move sync borrow release destroy swap rotate split join into state "
    "if else while return extern read write buffer true false"
).split()

failures = []


def check_files():
    files = sorted((ROOT / "examples").rglob("*.phase"))
    assert files, "no examples found"
    for f in files:
        text = f.read_text()
        tokens = tokenize(text)
        lines = text.split("\n")
        for n, line in enumerate(lines, 1):
            code = line.split("//")[0]
            for m in re.finditer(r"\b[A-Za-z_][A-Za-z0-9_]*\b", code):
                word = m.group(0)
                if word not in KEYWORDS:
                    continue
                if '"' in code[: m.start()]:
                    continue  # inside a string literal
                scopes = scopes_at(tokens, n, m.start(), line)
                if not any(
                    s.startswith(("keyword", "storage", "constant.language")) for s in scopes
                ):
                    failures.append(f"{f.relative_to(ROOT)}:{n}: '{word}' has scopes {scopes}")
    return len(files)


CASES = [
    # (source, line, text to find on it, scope expected to cover it)
    ("buffer<u8, 4> raw @DMA;", 1, "@DMA", "constant.other.domain.phase"),
    ("buffer<u8, 4> raw @DMA;", 1, "buffer", "storage.type.buffer.phase"),
    ("buffer<u8, 4> raw @DMA;", 1, "u8", "storage.type.primitive.phase"),
    ("fn main() {", 1, "main", "entity.name.function.phase"),
    ("    split ring into lo, hi;", 1, "split", "keyword.other.ownership.phase"),
    ("    split ring into lo, hi;", 1, "into", "keyword.other.ownership.phase"),
    ("    let r = borrow x read;", 1, "read", "storage.modifier.access.phase"),
    ("    volatile_write(Uart0.CTRL.TX_EN, 1);", 1, "volatile_write", "support.function.builtin.phase"),
    ("// just a comment with fn and move", 1, "move", "comment.line.double-slash.phase"),
    ('    log("fn move");', 1, "move", "string.quoted.double.phase"),
    ("device Uart0 @ 0x40004000 {\n    STATE: read u32 @ 0x04 { TXFULL: bit 0 }\n}", 1, "Uart0", "entity.name.type.device.phase"),
    ("device Uart0 @ 0x40004000 {\n    STATE: read u32 @ 0x04 { TXFULL: bit 0 }\n}", 1, "0x40004000", "constant.numeric.hex.phase"),
    ("device Uart0 @ 0x40004000 {\n    STATE: read u32 @ 0x04 { TXFULL: bit 0 }\n}", 2, "STATE", "variable.other.register.phase"),
    ("device Uart0 @ 0x40004000 {\n    STATE: read u32 @ 0x04 { TXFULL: bit 0 }\n}", 2, "bit", "keyword.other.bit.phase"),
    ("device D @ 0 {\n    A: read u32 @ 0 { X: bits 4..=7 }\n}\nfn main() { }", 4, "main", "entity.name.function.phase"),
    ("/* block\n   fn */ fn f() {}", 2, "f()", "entity.name.function.phase"),
    ('import "devices/board.phase";', 1, "import", "keyword.control.import.phase"),
    ("device type Uart {\n    DATA: read write u32 @ 0x00;\n}", 1, "type", "keyword.declaration.device.phase"),
    ("device type Uart {\n    DATA: read write u32 @ 0x00;\n}", 1, "Uart", "entity.name.type.device-type.phase"),
    ("device type Uart {\n    DATA: read write u32 @ 0x00;\n}", 2, "DATA", "variable.other.register.phase"),
    ("device Uart0: Uart @ 0x40004000;\nfn main() { move x -> @DMA; }", 1, "Uart0", "entity.name.type.device.phase"),
    ("event tick;", 1, "event", "keyword.declaration.event.phase"),
    ("interrupt fn on_t() @ irq 8 {\n    signal tick;\n}", 1, "interrupt", "keyword.declaration.interrupt.phase"),
    ("interrupt fn on_t() @ irq 8 {\n    signal tick;\n}", 1, "on_t", "entity.name.function.handler.phase"),
    ("interrupt fn on_t() @ irq 8 {\n    signal tick;\n}", 1, "irq", "keyword.other.irq.phase"),
    ("interrupt fn on_t() @ irq 8 {\n    signal tick;\n}", 2, "signal", "keyword.control.event.phase"),
    ("fn main() {\n    wait tick;\n    critical {\n    }\n}", 2, "wait", "keyword.control.event.phase"),
    ("fn main() {\n    wait tick;\n    critical {\n    }\n}", 3, "critical", "keyword.control.critical.phase"),
    ("fn main() {\n    wait ht for lo;\n}", 2, "wait", "keyword.control.event.phase"),
    ("fn main() {\n    wait ht for lo;\n}", 2, "ht", "variable.other.event.phase"),
    ("fn main() {\n    wait ht for lo;\n}", 2, "for", "keyword.control.event.phase"),
    ("const EN: u32 = 1 << 0;", 1, "const", "keyword.declaration.const.phase"),
    ("const EN: u32 = 1 << 0;", 1, "EN", "variable.other.constant.phase"),
    ("const EN: u32 = 1 << 0;", 1, "<<", "keyword.operator.phase"),
    # An instance line has no block: the next line is ordinary code again.
    ("device Uart0: Uart @ 0x40004000;\nfn main() { move x -> @DMA; }", 2, "main", "entity.name.function.phase"),
]


def check_cases():
    for src, line_no, needle, want in CASES:
        line = src.split("\n")[line_no - 1]
        col = line.rindex(needle) if needle == "f()" else line.index(needle)
        scopes = scopes_at(tokenize(src), line_no, col, line)
        if want not in scopes:
            failures.append(f"{src!r} line {line_no}: {needle!r} has {scopes}, want {want}")


n = check_files()
check_cases()
if failures:
    print("\n".join(failures))
    sys.exit(1)
print(f"grammar OK: {n} files, {len(CASES)} scope checks")
