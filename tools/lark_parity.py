# Lark's verdict on every VG-Sym file of the repository, for vg-core's parity test (docs/16, G1):
# tests/lark-parity.tsv holds one line per file: path, 1 or 0 for accept or reject, the number of
# tokens and the FNV-1a 64 of their texts joined by U+001F ("-" when the file does not lex).
#   target/venv/bin/python tools/lark_parity.py          rewrite the fixture
#   target/venv/bin/python tools/lark_parity.py --check  exit 1 when it is out of date
import pathlib, sys
from lark import Lark

root = pathlib.Path(__file__).resolve().parent.parent
grammar = Lark((root / "tools/vgsym.lark").read_text(), parser="earley", lexer="basic")

def fnv(texts):
    h = 0xcbf29ce484222325
    for b in "\x1f".join(texts).encode("utf-8"):
        h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"

files = sorted(p for d in ["examples", "tests/corpus", "tests/golden", "tests/lexical"]
               for p in (root / d).rglob("*.vgs"))
lines = []
for p in files:
    src = open(p, encoding="utf-8", newline="").read()
    try:
        texts = [t.value for t in grammar.lex(src)]
        toks, h = str(len(texts)), fnv(texts)
    except Exception:
        toks, h = "-", "-"
    try:
        grammar.parse(src)
        ok = "1"
    except Exception:
        ok = "0"
    lines.append(f"{p.relative_to(root)}\t{ok}\t{toks}\t{h}\n")
text = "".join(lines)
fixture = root / "tests/lark-parity.tsv"
if "--check" in sys.argv:
    if not fixture.exists() or fixture.read_text() != text:
        print("tests/lark-parity.tsv is out of date: run tools/lark_parity.py", file=sys.stderr)
        sys.exit(1)
    print(f"lark parity fixture current: {len(lines)} files")
else:
    fixture.write_text(text)
    print(f"wrote tests/lark-parity.tsv: {len(lines)} files")
