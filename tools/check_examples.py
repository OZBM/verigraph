# Parse every VG-Sym example in the repo against tools/vgsym.lark, plus the lexical cases that pin
# docs/03's lexical rules for vg-core's lexer. Run from the repo root:
#   pip install lark && python3 tools/check_examples.py
import re, sys, pathlib
from lark import Lark
g = Lark(pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "tools/vgsym.lark").read_text(), parser="earley", lexer="basic")
kit = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else "."); cases = []
for f in ["examples/audio_limiter.vgs", "examples/soft_knee_draft.vgs"]:
    cases.append((f, (kit / f).read_text(), True))
docs = sorted(str(p.relative_to(kit)) for p in (kit / "docs").rglob("*.md") if "source" not in p.parts)
for f in docs + ["skills/verigraph-synthesis/SKILL.md"]:
    for i, blk in enumerate(re.findall(r"```text\n(.*?)```", (kit / f).read_text(), re.S)):
        if "NODE" in blk:
            src = blk if blk.lstrip().startswith("module") else "module snippet;\n" + blk
            cases.append((f"{f}#{i}", src, True))
def node(params, post, body):
    return ("module lex;\nNODE @n {\n  SIGNATURE: (" + params + ") -> (i32);\n  EFFECTS: [];\n"
            "  CONTRACT { POST: " + post + "; }\n  BODY { " + body + " }\n}\n")
for name, src, ok in [
    ("x-1 is a subtraction", node("x: i32", "result == x-1", "RETURN(x);"), True),
    ("a literal keeps its sign", node("x: i8", "result == x", "%0 = ADD.wrap(x, -128:i8); RETURN(%0);"), True),
    ("an array literal", node("x: i32", "result == x", "%0 = IDENTITY([1, -2, 3]:Array<i32, 3>); %1 = INDEX(%0, 0:u64); RETURN(%1);"), True),
    ("x % 2 is a modulo", node("x: i32", "result == x % 2", "RETURN(x);"), True),
    ("%2 is always a port", node("x: i32", "result == x%2", "RETURN(x);"), False),
    ("(t.0).1 is two projections", node("x: i32", "(result.0).1 == x", "RETURN(x);"), True),
    ("t.0.1 lexes 0.1 as a number", node("x: i32", "result.0.1 == x", "RETURN(x);"), False),
    ("result is reserved", node("x: i32", "result == x", "%0 = ADD(result, x); RETURN(%0);"), False),
    ("post is reserved", node("post: i32", "result == post", "RETURN(post);"), False),
    ("in0 is one word", node("x: i32", "forall(i in0..2) { x > 0 }", "RETURN(x);"), False),
    ("a port may carry its type", node("x: i32", "result == x", "%0: i32 = ADD.wrap(x, 1:i32); RETURN(%0);"), True),
    ("fold: is a formula prefix", node("x: i32", "result == fold:step(0:i32, x, 1)", "RETURN(x);"), True),
    ("fold is reserved", node("fold: i32", "result == fold", "RETURN(fold);"), False),
]:
    cases.append(("lexical: " + name, src, ok))
bad = 0
for name, src, ok in cases:
    try:
        g.parse(src); err = None
    except Exception as e:
        err = str(e)[:300]
    if (err is None) == ok:
        print("ok  ", name)
    else:
        bad += 1; print("FAIL", name, err or "parsed, but must be rejected")
sys.exit(1 if bad else 0)
