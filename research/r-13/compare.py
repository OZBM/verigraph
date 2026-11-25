"""R-13: compare lifted Python functions with CPython on recorded inputs.

Usage: python3 -I compare.py CASES.jsonl SUMMARY.json
       python3 -I compare.py --kinds SOURCE.py FUNCTION   (prints the parameter kinds)
Each CASES line: {"path", "function", "params": ["int"|"bool"|"bytes"|"ints"],
                  "args": [...], "lifted": {"ok": int} | {"err": true}}
Classes per case:
  agree_value     lifted Ok(v) and CPython returned v
  agree_err       lifted Err and CPython raised
  err_overflow    lifted Err, CPython returned a value outside int64 (the contract's Err)
  err_non_int     lifted Err, CPython returned None or another non-int (the contract's Err)
  err_partial     lifted Err, CPython returned an int64 value (an intermediate left int64,
                  or a cure is conservative); allowed by the contract, reported separately
  err_unchecked   lifted Err on an argument above 2^20 in a function that shifts or raises to
                  a power, where CPython would build an int of up to 2^63 bits; not evaluated
  WRONG           lifted Ok(v) but CPython raised or returned something else, or the lift
                  trapped: a soundness bug
Every Ok result is checked: Ok means every intermediate fitted in int64, so CPython's do too.
"""

import importlib.util
import json
import sys

INT64_MIN, INT64_MAX = -(2**63), 2**63 - 1


def module_name(path):
    """The dotted name of a module under the stdlib or site-packages, else None."""
    import os
    import sysconfig

    for root in (sysconfig.get_paths()["stdlib"], sysconfig.get_paths()["purelib"]):
        rel = os.path.relpath(os.path.realpath(path), os.path.realpath(root))
        if not rel.startswith("..") and "site-packages" not in rel.split(os.sep)[:1]:
            parts = rel[: -len(".py")].split(os.sep)
            return ".".join(parts[:-1] if parts[-1] == "__init__" else parts)
    return None


def load(path, cache={}):
    """The module as CPython users get it: imported by name when it is installed (packages
    and relative imports work), else executed from its file."""
    if path not in cache and module_name(path):
        cache[path] = importlib.import_module(module_name(path))
    if path not in cache:
        spec = importlib.util.spec_from_file_location(f"r13_{len(cache)}", path)
        mod = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = mod  # enum.global_enum looks the module up here
        spec.loader.exec_module(mod)
        cache[path] = mod
    return cache[path]


def grows(fn, cache={}):
    """Whether fn shifts left or raises to a power, so one call can build an enormous int."""
    import inspect

    if fn not in cache:
        try:
            src = inspect.getsource(fn)
        except OSError:
            src = "<<"  # no source (a frozen module): assume the worst
        cache[fn] = "<<" in src or "**" in src or "pow(" in src
    return cache[fn]


def convert(arg, kind):
    if kind == "bool":
        return bool(arg)
    if kind == "bytes":
        return bytes(arg)
    if kind == "ints":
        return list(arg)
    return arg


def main(cases_path, summary_path):
    per_fn = {}
    examples = {}
    with open(cases_path, encoding="utf-8") as f:
        for line in f:
            c = json.loads(line)
            key = f"{c['path']}::{c['function']}"
            counts = per_fn.setdefault(key, dict.fromkeys(
                ["agree_value", "agree_err", "err_overflow", "err_non_int", "err_partial",
                 "err_unchecked", "WRONG"], 0))
            fn = getattr(load(c["path"]), c["function"])
            args = [convert(a, k) for a, k in zip(c["args"], c["params"])]
            if "err" in c["lifted"] and grows(fn) and any(
                    type(a) is int and abs(a) > 2**20 for a in args):
                counts["err_unchecked"] += 1
                continue
            try:
                want, raised = fn(*args), None
            except Exception as e:  # any exception is the lift's Err
                want, raised = None, type(e).__name__
            got = c["lifted"]
            if "trap" in got:
                cls = "WRONG"  # a trap is neither CPython's value nor Err
            elif "err" in got:
                if raised:
                    cls = "agree_err"
                elif type(want) not in (int, bool):
                    cls = "err_non_int"
                elif not INT64_MIN <= want <= INT64_MAX:
                    cls = "err_overflow"
                else:
                    cls = "err_partial"
            elif raised is None and type(want) in (int, bool) and want == got["ok"]:
                cls = "agree_value"
            else:
                cls = "WRONG"
            counts[cls] += 1
            if cls in ("WRONG", "err_partial") and (key, cls) not in examples:
                examples[(key, cls)] = {"args": c["args"], "lifted": got,
                                        "cpython": raised or repr(want)}
    out = {"functions": per_fn,
           "examples": [{"function": k, "class": cls, **v} for (k, cls), v in sorted(examples.items())]}
    with open(summary_path, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1, sort_keys=True)
        f.write("\n")
    wrong = sum(c["WRONG"] for c in per_fn.values())
    print(f"{len(per_fn)} functions, {wrong} WRONG cases")
    return 1 if wrong else 0


def param_kinds(fn):
    """Parameter kinds as pylift reads the annotations (also when they are strings, under
    `from __future__ import annotations`): int, bool, bytes or ints."""
    import inspect

    out = []
    for p in inspect.signature(fn).parameters.values():
        a = p.annotation
        name = a if isinstance(a, str) else getattr(a, "__name__", None)
        if a is inspect.Parameter.empty or name == "int":
            out.append("int")
        elif name in ("bool", "bytes"):
            out.append(name)
        else:
            out.append("ints")  # list[int], tuple[int, ...]; pylift rejects anything else
    return out


def kinds(path, function):
    print(json.dumps(param_kinds(getattr(load(path), function))))
    return 0


if __name__ == "__main__":
    if sys.argv[1] == "--kinds":
        sys.exit(kinds(*sys.argv[2:]))
    sys.exit(main(*sys.argv[1:]))
