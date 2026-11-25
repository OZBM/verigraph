"""R-13: check pylift's C against CPython natively, without the lifter.

Usage: python3 -I native_check.py SOURCE.py FUNCTION CASES SEED
Translates FUNCTION, compiles the C into a shared library with a probe that returns the
value and the error bit, and compares each case with CPython:
  agree      error bit and CPython raising (or leaving int64) coincide, and values match
  partial    error bit set but CPython returned an int64 value (an intermediate left int64)
  WRONG      anything else: a translation bug
Prints one JSON line with the counts and the first WRONG case. Exit 1 on any WRONG.
"""

import ctypes
import importlib.util
import inspect
import json
import os
import random
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
PYLIFT = os.path.join(HERE, "..", "..", "crates", "vg-omnilift", "src", "pylift.py")
CLANG = "/opt/homebrew/opt/llvm/bin/clang"
INT64_MIN, INT64_MAX = -(2**63), 2**63 - 1
EDGES = [0, 1, -1, 2, -2, 3, 7, 8, 63, 64, 65, -63, -64, -65, 2**31 - 1, -(2**31), 2**32,
         2**62, -(2**62), INT64_MAX, INT64_MIN, INT64_MAX - 1, INT64_MIN + 1]


def draw_int(rng, small):
    if small:
        return rng.randint(-64, 2048)
    k = rng.random()
    if k < 0.3:
        return rng.randint(-16, 16)
    if k < 0.55:
        return rng.randint(-(2**16), 2**16)
    if k < 0.8:
        return rng.choice(EDGES)
    return rng.randint(INT64_MIN, INT64_MAX)


def draw(rng, kind, small):
    if kind == "bool":
        return rng.random() < 0.5
    if kind == "bytes":
        return bytes(rng.randrange(256) for _ in range(rng.randint(0, 12)))
    if kind == "ints":
        return [draw_int(rng, True) for _ in range(rng.randint(0, 8))]
    return draw_int(rng, small)


def main(src, name, cases, seed):
    cases, rng = int(cases), random.Random(int(seed))
    sys.dont_write_bytecode = True  # no __pycache__ in the repository
    sys.path.insert(0, HERE)
    from compare import load, param_kinds  # installed modules by dotted name, others by file

    fn = getattr(load(src), name)
    kinds = param_kinds(fn)
    with tempfile.TemporaryDirectory() as d:
        c_path, so_path = os.path.join(d, "f.c"), os.path.join(d, "f.so")
        subprocess.run([sys.executable, "-I", PYLIFT, src, name, c_path], check=True)
        params, args = [], []
        for i, k in enumerate(kinds):
            if k in ("bytes", "ints"):
                elem = "uint8_t" if k == "bytes" else "int64_t"
                params += [f"const {elem} *a{i}", f"size_t n{i}"]
                args += [f"a{i}", f"n{i}"]
            else:
                params.append(f"int64_t a{i}")
                args.append(f"a{i}")
        with open(c_path, "a") as f:
            f.write(f"int64_t probe({', '.join(params + ['int64_t *e'])}) "
                    f"{{ *e = 0; return py_impl_{name}({', '.join(args + ['e'])}); }}\n")
        subprocess.run([CLANG, "-O0", "-shared", "-Wno-unused-function", "-o", so_path, c_path],
                       check=True)
        lib = ctypes.CDLL(so_path)
        try:
            small = "range(" in inspect.getsource(fn)  # an int-driven loop: keep counts small
        except OSError:
            small = True  # no source (a frozen module): assume a loop
        counts = {"agree": 0, "partial": 0, "WRONG": 0}
        first_wrong = None
        for _ in range(cases):
            vals = [draw(rng, k, small) for k in kinds]
            cargs = []
            for v, k in zip(vals, kinds):
                if k == "bytes":
                    cargs += [ctypes.create_string_buffer(v, max(len(v), 1)), ctypes.c_size_t(len(v))]
                elif k == "ints":
                    cargs += [(ctypes.c_int64 * max(len(v), 1))(*v), ctypes.c_size_t(len(v))]
                else:
                    cargs.append(ctypes.c_int64(int(v)))
            e = ctypes.c_int64(0)
            lib.probe.restype = ctypes.c_int64
            got = lib.probe(*cargs, ctypes.byref(e))
            try:
                want, raised = fn(*vals), False
            except Exception:
                want, raised = None, True
            fits = not raised and type(want) in (int, bool) and INT64_MIN <= want <= INT64_MAX
            if e.value:
                cls = "partial" if fits else "agree"
            else:
                cls = "agree" if fits and want == got else "WRONG"
            counts[cls] += 1
            if cls == "WRONG" and first_wrong is None:
                first_wrong = {"args": [list(v) if isinstance(v, bytes) else v for v in vals],
                               "c": got, "cpython": "raised" if raised else repr(want)}
    print(json.dumps({"function": name, **counts, "first_wrong": first_wrong}))
    return 1 if counts["WRONG"] else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:]))
