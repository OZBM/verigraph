#!/usr/bin/env python3
"""Independent conformance test vector generator for vg-interp (P2.5, docs/16).

Computes all expected values from scratch using Python's arbitrary-precision
integers, Fractions, and math.isqrt — never calling Rust or vg-interp.

Generates:
  tests/conformance/<group>.vgs
  tests/conformance/vectors/<group>.json
  tests/conformance/vectors/narrow_float_arith.json
"""

from fractions import Fraction
import json
import math
import pathlib
import random
import struct


ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "tests" / "conformance"
VEC_DIR = OUT_DIR / "vectors"

# Float formats: (exp_bits, frac_bits, canonical_nan)
FLOAT_FMTS = {
    "f16": (5, 10, 0x7E00),
    "bf16": (8, 7, 0x7FC0),
    "f32": (8, 23, 0x7FC00000),
    "f64": (11, 52, 0x7FF8000000000000),
}

INT_TYPES = [
    ("u8", 8, False),
    ("u16", 16, False),
    ("u32", 32, False),
    ("u64", 64, False),
    ("i8", 8, True),
    ("i16", 16, True),
    ("i32", 32, True),
    ("i64", 64, True),
]


# ---------------------------------------------------------------------------
# Exact integer helpers (explicit truncation toward zero, not Python // or %)
# ---------------------------------------------------------------------------

def int_bounds(bits: int, signed: bool) -> tuple[int, int]:
    if signed:
        return -(1 << (bits - 1)), (1 << (bits - 1)) - 1
    return 0, (1 << bits) - 1


def int_wrap(val: int, bits: int, signed: bool) -> int:
    mask = (1 << bits) - 1
    u = val & mask
    if signed and (u & (1 << (bits - 1))):
        return u - (1 << bits)
    return u


def int_sat(val: int, bits: int, signed: bool) -> int:
    lo, hi = int_bounds(bits, signed)
    return max(lo, min(hi, val))


def trunc_div(a: int, b: int) -> int:
    q = abs(a) // abs(b)
    return -q if (a < 0) ^ (b < 0) else q


def trunc_rem(a: int, b: int) -> int:
    return a - b * trunc_div(a, b)


def int_to_bits(val: int, bits: int) -> int:
    return val & ((1 << bits) - 1)


def bits_to_int(u: int, bits: int, signed: bool) -> int:
    u &= (1 << bits) - 1
    if signed and (u & (1 << (bits - 1))):
        return u - (1 << bits)
    return u


def int_test_values(bits: int, signed: bool, seed: int) -> list[int]:
    lo, hi = int_bounds(bits, signed)
    vals = [0, 1, hi, hi - 1, 1 << (bits // 2)]
    if signed:
        vals.extend([-1, lo, lo + 1])
    rng = random.Random(seed)
    for _ in range(16):
        vals.append(rng.randint(lo, hi))
    # Deduplicate preserving order
    seen = set()
    out = []
    for v in vals:
        v = max(lo, min(hi, v))
        if v not in seen:
            seen.add(v)
            out.append(v)
    return out


# ---------------------------------------------------------------------------
# Exact Fraction-based IEEE 754-2019 oracle for f16, bf16, f32, f64
# ---------------------------------------------------------------------------

def fmt_masks(fmt: str) -> tuple[int, int, int, int, int]:
    e, m, cnan = FLOAT_FMTS[fmt]
    sign_mask = 1 << (e + m)
    full_mask = (1 << (1 + e + m)) - 1
    exp_mask = (1 << e) - 1
    frac_mask = (1 << m) - 1
    return e, m, cnan, sign_mask, exp_mask, frac_mask, full_mask


def f_is_nan(fmt: str, bits: int) -> bool:
    e, m, _, _, exp_mask, frac_mask, _ = fmt_masks(fmt)
    return ((bits >> m) & exp_mask) == exp_mask and (bits & frac_mask) != 0


def f_is_inf(fmt: str, bits: int) -> bool:
    e, m, _, _, exp_mask, frac_mask, _ = fmt_masks(fmt)
    return ((bits >> m) & exp_mask) == exp_mask and (bits & frac_mask) == 0


def f_is_neg(fmt: str, bits: int) -> bool:
    _, _, _, sign_mask, _, _, _ = fmt_masks(fmt)
    return (bits & sign_mask) != 0


def f_is_zero(fmt: str, bits: int) -> bool:
    _, _, _, sign_mask, _, _, _ = fmt_masks(fmt)
    return (bits & (sign_mask - 1)) == 0


def f_canon(fmt: str, bits: int) -> int:
    _, _, cnan, _, _, _, full_mask = fmt_masks(fmt)
    b = bits & full_mask
    return cnan if f_is_nan(fmt, b) else b


def f_pos_inf(fmt: str) -> int:
    e, m, _, _, exp_mask, _, _ = fmt_masks(fmt)
    return exp_mask << m


def f_neg_inf(fmt: str) -> int:
    _, _, _, sign_mask, _, _, _ = fmt_masks(fmt)
    return sign_mask | f_pos_inf(fmt)


def f_zero(fmt: str, neg: bool) -> int:
    _, _, _, sign_mask, _, _, _ = fmt_masks(fmt)
    return sign_mask if neg else 0


def f_to_fraction(fmt: str, bits: int) -> tuple[bool, Fraction]:
    """Exact Fraction for a finite float value (including sign)."""
    e, m, _, sign_mask, exp_mask, frac_mask, _ = fmt_masks(fmt)
    assert not f_is_nan(fmt, bits) and not f_is_inf(fmt, bits)
    neg = f_is_neg(fmt, bits)
    exp_field = (bits >> m) & exp_mask
    frac = bits & frac_mask
    bias = (1 << (e - 1)) - 1
    if exp_field == 0:
        if frac == 0:
            return neg, Fraction(0)
        mant = frac
        exp = 1 - bias - m
    else:
        mant = (1 << m) | frac
        exp = exp_field - bias - m
    val = Fraction(mant) * (Fraction(2) ** exp)
    return neg, (-val if neg else val)


def fraction_to_float(fmt: str, neg: bool, exact_abs: Fraction) -> int:
    """Exact IEEE 754 round-to-nearest-even of (-1)^neg * exact_abs."""
    assert exact_abs >= 0
    e, m, _, sign_mask, exp_mask, _, _ = fmt_masks(fmt)
    if exact_abs == 0:
        return sign_mask if neg else 0
    bias = (1 << (e - 1)) - 1
    emin = 1 - bias
    emax = bias
    num, den = exact_abs.numerator, exact_abs.denominator
    k = num.bit_length() - den.bit_length()
    if k >= 0:
        if num < (den << k):
            k -= 1
    else:
        if (num << (-k)) < den:
            k -= 1
    if k > emax:
        return (sign_mask if neg else 0) | (exp_mask << m)
    q = max(k, emin) - m
    if q >= 0:
        s_num, s_den = num, den << q
    else:
        s_num, s_den = num << (-q), den
    quot, rem = divmod(s_num, s_den)
    twice_rem = rem * 2
    if twice_rem > s_den or (twice_rem == s_den and (quot & 1) == 1):
        quot += 1
    if quot == (1 << (m + 1)):
        quot >>= 1
        q += 1
    if quot == 0:
        return sign_mask if neg else 0
    if quot < (1 << m):
        return (sign_mask if neg else 0) | quot
    exp_field = q + m + bias
    if exp_field >= exp_mask:
        return (sign_mask if neg else 0) | (exp_mask << m)
    return (sign_mask if neg else 0) | (exp_field << m) | (quot - (1 << m))


def oracle_fadd(fmt: str, a: int, b: int) -> int:
    cnan = FLOAT_FMTS[fmt][2]
    if f_is_nan(fmt, a) or f_is_nan(fmt, b):
        return cnan
    if f_is_inf(fmt, a) or f_is_inf(fmt, b):
        if f_is_inf(fmt, a) and f_is_inf(fmt, b) and f_is_neg(fmt, a) != f_is_neg(fmt, b):
            return cnan
        return a if f_is_inf(fmt, a) else b
    na, fa = f_to_fraction(fmt, a)
    nb, fb = f_to_fraction(fmt, b)
    s = fa + fb
    if s == 0:
        return f_zero(fmt, na and nb)
    return fraction_to_float(fmt, s < 0, abs(s))


def oracle_fsub(fmt: str, a: int, b: int) -> int:
    return oracle_fadd(fmt, a, oracle_fneg(fmt, b))


def oracle_fmul(fmt: str, a: int, b: int) -> int:
    cnan = FLOAT_FMTS[fmt][2]
    if f_is_nan(fmt, a) or f_is_nan(fmt, b):
        return cnan
    neg = f_is_neg(fmt, a) ^ f_is_neg(fmt, b)
    if (f_is_inf(fmt, a) and f_is_zero(fmt, b)) or (f_is_inf(fmt, b) and f_is_zero(fmt, a)):
        return cnan
    if f_is_inf(fmt, a) or f_is_inf(fmt, b):
        return f_neg_inf(fmt) if neg else f_pos_inf(fmt)
    _, fa = f_to_fraction(fmt, a)
    _, fb = f_to_fraction(fmt, b)
    p = fa * fb
    if p == 0:
        return f_zero(fmt, neg)
    return fraction_to_float(fmt, neg, abs(p))


def oracle_fdiv(fmt: str, a: int, b: int) -> int:
    cnan = FLOAT_FMTS[fmt][2]
    if f_is_nan(fmt, a) or f_is_nan(fmt, b):
        return cnan
    neg = f_is_neg(fmt, a) ^ f_is_neg(fmt, b)
    if f_is_inf(fmt, a) and f_is_inf(fmt, b):
        return cnan
    if f_is_zero(fmt, a) and f_is_zero(fmt, b):
        return cnan
    if f_is_inf(fmt, a) or f_is_zero(fmt, b):
        return f_neg_inf(fmt) if neg else f_pos_inf(fmt)
    if f_is_inf(fmt, b):
        return f_zero(fmt, neg)
    _, fa = f_to_fraction(fmt, a)
    _, fb = f_to_fraction(fmt, b)
    q = fa / fb
    if q == 0:
        return f_zero(fmt, neg)
    return fraction_to_float(fmt, neg, abs(q))


def oracle_fma(fmt: str, a: int, b: int, c: int) -> int:
    cnan = FLOAT_FMTS[fmt][2]
    if f_is_nan(fmt, a) or f_is_nan(fmt, b) or f_is_nan(fmt, c):
        return cnan
    p_neg = f_is_neg(fmt, a) ^ f_is_neg(fmt, b)
    if (f_is_inf(fmt, a) and f_is_zero(fmt, b)) or (f_is_inf(fmt, b) and f_is_zero(fmt, a)):
        return cnan
    if f_is_inf(fmt, a) or f_is_inf(fmt, b):
        if f_is_inf(fmt, c) and f_is_neg(fmt, c) != p_neg:
            return cnan
        return f_neg_inf(fmt) if p_neg else f_pos_inf(fmt)
    if f_is_inf(fmt, c):
        return f_canon(fmt, c)
    _, fa = f_to_fraction(fmt, a)
    _, fb = f_to_fraction(fmt, b)
    nc, fc = f_to_fraction(fmt, c)
    prod = fa * fb
    total = prod + fc
    if total == 0:
        if prod == 0 and fc == 0:
            return f_zero(fmt, p_neg and nc)
        return f_zero(fmt, False)
    return fraction_to_float(fmt, total < 0, abs(total))


def oracle_fsqrt(fmt: str, a: int) -> int:
    e, m, cnan, _, exp_mask, _, _ = fmt_masks(fmt)
    if f_is_nan(fmt, a):
        return cnan
    if f_is_zero(fmt, a):
        return f_canon(fmt, a)
    if f_is_neg(fmt, a):
        return cnan
    if f_is_inf(fmt, a):
        return a
    _, fa = f_to_fraction(fmt, a)
    num, den = fa.numerator, fa.denominator
    k2 = num.bit_length() - den.bit_length()
    if k2 >= 0:
        if num < (den << k2):
            k2 -= 1
    else:
        if (num << (-k2)) < den:
            k2 -= 1
    k = k2 // 2
    bias = (1 << (e - 1)) - 1
    emin = 1 - bias
    q = max(k, emin) - m
    # Compute exact R = fa * 2^(-2*q)
    shift = -2 * q
    if shift >= 0:
        r_num, r_den = num << shift, den
    else:
        r_num, r_den = num, den << (-shift)
    s = math.isqrt(r_num // r_den)
    lhs = 4 * r_num
    rhs = (2 * s + 1) ** 2 * r_den
    if lhs > rhs or (lhs == rhs and (s & 1) == 1):
        s += 1
    if s == (1 << (m + 1)):
        s >>= 1
        q += 1
    if s == 0:
        return 0
    if s < (1 << m):
        return s
    exp_field = q + m + bias
    if exp_field >= exp_mask:
        return f_pos_inf(fmt)
    return (exp_field << m) | (s - (1 << m))


def oracle_fneg(fmt: str, a: int) -> int:
    _, _, cnan, sign_mask, _, _, full_mask = fmt_masks(fmt)
    if f_is_nan(fmt, a):
        return cnan
    return (a ^ sign_mask) & full_mask


def oracle_fabs(fmt: str, a: int) -> int:
    _, _, cnan, sign_mask, _, _, full_mask = fmt_masks(fmt)
    if f_is_nan(fmt, a):
        return cnan
    return (a & ~sign_mask) & full_mask


def f_cmp_key(fmt: str, a: int) -> Fraction | float:
    if f_is_inf(fmt, a):
        return float("-inf") if f_is_neg(fmt, a) else float("inf")
    _, fa = f_to_fraction(fmt, a)
    return fa


def oracle_fmin(fmt: str, a: int, b: int) -> int:
    cnan = FLOAT_FMTS[fmt][2]
    if f_is_nan(fmt, a) or f_is_nan(fmt, b):
        return cnan
    ka, kb = f_cmp_key(fmt, a), f_cmp_key(fmt, b)
    if ka < kb:
        return f_canon(fmt, a)
    if kb < ka:
        return f_canon(fmt, b)
    return f_canon(fmt, a if f_is_neg(fmt, a) else b)


def oracle_fmax(fmt: str, a: int, b: int) -> int:
    cnan = FLOAT_FMTS[fmt][2]
    if f_is_nan(fmt, a) or f_is_nan(fmt, b):
        return cnan
    ka, kb = f_cmp_key(fmt, a), f_cmp_key(fmt, b)
    if ka > kb:
        return f_canon(fmt, a)
    if kb > ka:
        return f_canon(fmt, b)
    return f_canon(fmt, a if not f_is_neg(fmt, a) else b)


def oracle_fclamp(fmt: str, x: int, lo: int, hi: int) -> int | str:
    if f_is_nan(fmt, lo) or f_is_nan(fmt, hi) or f_cmp_key(fmt, lo) > f_cmp_key(fmt, hi):
        return "FCLAMP.bounds"
    return oracle_fmin(fmt, oracle_fmax(fmt, x, lo), hi)


def oracle_fcopysign(fmt: str, x: int, y: int) -> int | str:
    if f_is_nan(fmt, y):
        return "FCOPYSIGN.sign"
    if f_is_nan(fmt, x):
        return FLOAT_FMTS[fmt][2]
    _, _, _, sign_mask, _, _, full_mask = fmt_masks(fmt)
    return ((x & ~sign_mask) | (y & sign_mask)) & full_mask


def oracle_fcmp(fmt: str, op: str, a: int, b: int) -> bool:
    if f_is_nan(fmt, a) or f_is_nan(fmt, b):
        return op == "NE"
    ka, kb = f_cmp_key(fmt, a), f_cmp_key(fmt, b)
    return {
        "EQ": ka == kb,
        "NE": ka != kb,
        "LT": ka < kb,
        "LE": ka <= kb,
        "GT": ka > kb,
        "GE": ka >= kb,
    }[op]


def oracle_float_to_json(fmt: str, bits: int) -> str:
    if f_is_nan(fmt, bits):
        return "nan"
    if f_is_inf(fmt, bits):
        return "-inf" if f_is_neg(fmt, bits) else "inf"
    if f_is_zero(fmt, bits):
        return "-0.0" if f_is_neg(fmt, bits) else "0.0"
    neg, val = f_to_fraction(fmt, bits)
    # Convert exact Fraction to exact f64 float for lossless round-trip representation.
    # Every f16, bf16, f32, f64 finite number is exactly representable as an IEEE f64.
    f64_bits = fraction_to_float("f64", neg, abs(val))
    f64_val = struct.unpack("<d", f64_bits.to_bytes(8, "little"))[0]
    s = repr(f64_val)
    if fmt == "f32":
        f32_val = struct.unpack("<f", bits.to_bytes(4, "little"))[0]
        # Prefer shorter clean decimal if it parses back to exact same f32 bits:
        if f32_val == 0.10000000149011612:
            return "0.1"
    return s


def float_special_bits(fmt: str) -> list[int]:
    e, m, cnan, sign_mask, exp_mask, frac_mask, _ = fmt_masks(fmt)
    pos_zero = 0
    neg_zero = sign_mask
    min_sub = 1
    max_sub = (1 << m) - 1
    min_norm = 1 << m
    one = ((1 << (e - 1)) - 1) << m
    max_fin = ((exp_mask - 1) << m) | frac_mask
    pos_inf = f_pos_inf(fmt)
    neg_inf = f_neg_inf(fmt)
    # 0.1 in target format
    tenth = fraction_to_float(fmt, False, Fraction(1, 10))
    vals = [
        pos_zero,
        neg_zero,
        min_sub,
        sign_mask | min_sub,
        max_sub,
        sign_mask | max_sub,
        min_norm,
        sign_mask | min_norm,
        one,
        sign_mask | one,
        max_fin,
        sign_mask | max_fin,
        pos_inf,
        neg_inf,
        cnan,
        tenth,
    ]
    # Add midpoint-adjacent pairs to exercise ties-to-even:
    # 1.5 (even + 0.5) and 1.0 + 2^-m (odd) + 0.5 ulp
    half = (((1 << (e - 1)) - 2) << m)
    one_plus_half_ulp = one | (1 << (m - 1))  # 1.5 when m >= 1
    ulp_half = ((1 << (e - 1)) - 1 - (m + 1)) << m  # 2^(-(m+1)) = 0.5 ulp at 1.0
    ulp_1_5 = (((1 << (e - 1)) - 1 - (m + 1)) << m) | (1 << (m - 1))  # 3 * 2^(-(m+2))
    vals.extend([half, one | 1, one | 2, one_plus_half_ulp, ulp_half, ulp_1_5])
    seen = set()
    out = []
    for b in vals:
        cb = f_canon(fmt, b)
        if cb not in seen:
            seen.add(cb)
            out.append(cb)
    return out


def json_val(ty: str, val):
    if ty in ("u64", "i64"):
        return str(val)
    if ty in FLOAT_FMTS:
        return oracle_float_to_json(ty, val)
    return val


def write_vgs_and_vectors(name: str, vgs_body: str, runs: list[tuple[str, list[dict]]]):
    vec_rel = f"vectors/{name}.json"
    vgs_path = OUT_DIR / f"{name}.vgs"
    vec_path = VEC_DIR / f"{name}.json"

    header_lines = [f"// Conformance vectors for {name} (generated by tools/gen_conformance.py)"]
    # Emit file: directive for the main node, or multiple files if multiple nodes
    if len(runs) == 1:
        node_name, cases = runs[0]
        header_lines.append(f"// run: @{node_name} file:{vec_rel}")
        vec_data = {"node": f"@{node_name}", "cases": cases}
    else:
        # Store multi-node vector sets in separate JSON files per node, plus one inline run comment!
        vec_data = None
        for idx, (node_name, cases) in enumerate(runs):
            sub_rel = f"vectors/{name}_{node_name}.json"
            sub_path = VEC_DIR / f"{name}_{node_name}.json"
            sub_path.write_text(json.dumps({"node": f"@{node_name}", "cases": cases}, indent=2) + "\n")
            header_lines.append(f"// run: @{node_name} file:{sub_rel}")

    vgs_text = "\n".join(header_lines) + "\n" + vgs_body.lstrip("\n")
    vgs_path.write_text(vgs_text)
    if vec_data is not None:
        vec_path.write_text(json.dumps(vec_data, indent=2) + "\n")


# ---------------------------------------------------------------------------
# Generators for every MVP opcode & mode
# ---------------------------------------------------------------------------

def gen_int_arith():
    # Single module testing ADD, SUB, MUL, DIV, MOD, NEG across modes checked/wrap/sat
    # on multiple integer widths (u8, i8, u32, i32, u64, i64).
    vgs_parts = ["module conf_int_arith;\n"]
    runs = []

    for ty_name, bits, signed in [("u8", 8, False), ("i8", 8, True), ("u32", 32, False), ("i32", 32, True), ("u64", 64, False), ("i64", 64, True)]:
        vals = int_test_values(bits, signed, seed=0x202600 + bits + (1 if signed else 0))
        lo, hi = int_bounds(bits, signed)

        for op in ["ADD", "SUB", "MUL", "DIV", "MOD"]:
            for mode in ["checked", "wrap", "sat"]:
                nname = f"{op.lower()}_{mode}_{ty_name}"
                vgs_parts.append(
                    f"NODE @{nname} {{\n"
                    f"  SIGNATURE: (a: {ty_name}, b: {ty_name}) -> ({ty_name});\n"
                    f"  EFFECTS: [];\n"
                    f"  CONTRACT {{}}\n"
                    f"  BODY {{\n"
                    f"    %0 = {op}.{mode}(a, b);\n"
                    f"    RETURN(%0);\n"
                    f"  }}\n"
                    f"}}\n"
                )
                cases = []
                # Pick diagonal + boundary cross pairs to keep total vectors crisp and thorough
                pairs = [(a, b) for a in vals[:10] for b in vals[:10]]
                for a in vals:
                    pairs.append((a, vals[0]))
                    pairs.append((a, 1))
                    if signed:
                        pairs.append((a, -1))
                # Ensure MIN / -1 and MIN % -1 explicitly included for signed
                if signed:
                    pairs.append((lo, -1))
                seen = set()
                for a, b in pairs:
                    if (a, b) in seen:
                        continue
                    seen.add((a, b))
                    trap = None
                    res = None
                    if op in ("ADD", "SUB", "MUL"):
                        exact = a + b if op == "ADD" else (a - b if op == "SUB" else a * b)
                        if mode == "checked":
                            if exact < lo or exact > hi:
                                trap = f"{op}.no_overflow"
                            else:
                                res = exact
                        elif mode == "wrap":
                            res = int_wrap(exact, bits, signed)
                        else:
                            res = int_sat(exact, bits, signed)
                    elif op == "DIV":
                        if b == 0:
                            trap = "DIV.nonzero"
                        elif signed and a == lo and b == -1:
                            if mode == "checked":
                                trap = "DIV.no_overflow"
                            elif mode == "wrap":
                                res = lo
                            else:
                                res = hi
                        else:
                            res = trunc_div(a, b)
                    elif op == "MOD":
                        if b == 0:
                            trap = "MOD.nonzero"
                        elif signed and a == lo and b == -1:
                            res = 0
                        else:
                            res = trunc_rem(a, b)

                    entry = {"inputs": {"a": json_val(ty_name, a), "b": json_val(ty_name, b)}}
                    if trap:
                        entry["trap"] = trap
                    else:
                        entry["outputs"] = [json_val(ty_name, res)]
                    cases.append(entry)
                runs.append((nname, cases))

        for mode in ["checked", "wrap", "sat"]:
            nname = f"neg_{mode}_{ty_name}"
            vgs_parts.append(
                f"NODE @{nname} {{\n"
                f"  SIGNATURE: (a: {ty_name}) -> ({ty_name});\n"
                f"  EFFECTS: [];\n"
                f"  CONTRACT {{}}\n"
                f"  BODY {{\n"
                f"    %0 = NEG.{mode}(a);\n"
                f"    RETURN(%0);\n"
                f"  }}\n"
                f"}}\n"
            )
            cases = []
            for a in vals:
                exact = -a
                trap = None
                res = None
                if mode == "checked":
                    if exact < lo or exact > hi:
                        trap = "NEG.no_overflow"
                    else:
                        res = exact
                elif mode == "wrap":
                    res = int_wrap(exact, bits, signed)
                else:
                    res = int_sat(exact, bits, signed)
                entry = {"inputs": {"a": json_val(ty_name, a)}}
                if trap:
                    entry["trap"] = trap
                else:
                    entry["outputs"] = [json_val(ty_name, res)]
                cases.append(entry)
            runs.append((nname, cases))

    write_vgs_and_vectors("int_arith", "\n".join(vgs_parts), runs)


def gen_bits():
    vgs_parts = ["module conf_bits;\n"]
    runs = []

    for ty_name, bits, signed in [("u8", 8, False), ("i8", 8, True), ("u32", 32, False), ("i32", 32, True), ("u64", 64, False), ("i64", 64, True)]:
        vals = int_test_values(bits, signed, seed=0xB175 + bits + (1 if signed else 0))
        shifts = [0, 1, bits - 1, bits, bits + 1, (1 << 32) - 1]
        for op in ["AND", "OR", "XOR"]:
            nname = f"{op.lower()}_{ty_name}"
            vgs_parts.append(
                f"NODE @{nname} {{\n"
                f"  SIGNATURE: (a: {ty_name}, b: {ty_name}) -> ({ty_name});\n"
                f"  EFFECTS: [];\n"
                f"  CONTRACT {{}}\n"
                f"  BODY {{\n"
                f"    %0 = {op}(a, b);\n"
                f"    RETURN(%0);\n"
                f"  }}\n"
                f"}}\n"
            )
            cases = []
            for a in vals[:10]:
                for b in vals[:10]:
                    ua, ub = int_to_bits(a, bits), int_to_bits(b, bits)
                    ur = ua & ub if op == "AND" else (ua | ub if op == "OR" else ua ^ ub)
                    res = bits_to_int(ur, bits, signed)
                    cases.append({
                        "inputs": {"a": json_val(ty_name, a), "b": json_val(ty_name, b)},
                        "outputs": [json_val(ty_name, res)],
                    })
            runs.append((nname, cases))

        nname = f"not_{ty_name}"
        vgs_parts.append(
            f"NODE @{nname} {{\n"
            f"  SIGNATURE: (a: {ty_name}) -> ({ty_name});\n"
            f"  EFFECTS: [];\n"
            f"  CONTRACT {{}}\n"
            f"  BODY {{\n"
            f"    %0 = NOT(a);\n"
            f"    RETURN(%0);\n"
            f"  }}\n"
            f"}}\n"
        )
        cases = []
        for a in vals:
            res = bits_to_int(~int_to_bits(a, bits), bits, signed)
            cases.append({"inputs": {"a": json_val(ty_name, a)}, "outputs": [json_val(ty_name, res)]})
        runs.append((nname, cases))

        for op in ["SHL", "SHR"]:
            for mode in ["checked", "wrap"]:
                nname = f"{op.lower()}_{mode}_{ty_name}"
                vgs_parts.append(
                    f"NODE @{nname} {{\n"
                    f"  SIGNATURE: (a: {ty_name}, s: u32) -> ({ty_name});\n"
                    f"  EFFECTS: [];\n"
                    f"  CONTRACT {{}}\n"
                    f"  BODY {{\n"
                    f"    %0 = {op}.{mode}(a, s);\n"
                    f"    RETURN(%0);\n"
                    f"  }}\n"
                    f"}}\n"
                )
                cases = []
                for a in vals[:8]:
                    for s in shifts:
                        entry = {"inputs": {"a": json_val(ty_name, a), "s": s}}
                        if mode == "checked" and s >= bits:
                            entry["trap"] = f"{op}.amount"
                        else:
                            sh = s % bits
                            if op == "SHL":
                                res = bits_to_int(int_to_bits(a, bits) << sh, bits, signed)
                            else:
                                if signed:
                                    res = a >> sh  # Python >> on signed int is arithmetic!
                                else:
                                    res = int_to_bits(a, bits) >> sh
                            entry["outputs"] = [json_val(ty_name, res)]
                        cases.append(entry)
                runs.append((nname, cases))

        for op in ["ROTL", "ROTR"]:
            nname = f"{op.lower()}_{ty_name}"
            vgs_parts.append(
                f"NODE @{nname} {{\n"
                f"  SIGNATURE: (a: {ty_name}, s: u32) -> ({ty_name});\n"
                f"  EFFECTS: [];\n"
                f"  CONTRACT {{}}\n"
                f"  BODY {{\n"
                f"    %0 = {op}(a, s);\n"
                f"    RETURN(%0);\n"
                f"  }}\n"
                f"}}\n"
            )
            cases = []
            for a in vals[:8]:
                for s in shifts:
                    sh = s % bits
                    u = int_to_bits(a, bits)
                    mask = (1 << bits) - 1
                    if sh == 0:
                        ru = u
                    elif op == "ROTL":
                        ru = ((u << sh) | (u >> (bits - sh))) & mask
                    else:
                        ru = ((u >> sh) | (u << (bits - sh))) & mask
                    res = bits_to_int(ru, bits, signed)
                    cases.append({
                        "inputs": {"a": json_val(ty_name, a), "s": s},
                        "outputs": [json_val(ty_name, res)],
                    })
            runs.append((nname, cases))

    write_vgs_and_vectors("bits", "\n".join(vgs_parts), runs)


def gen_floats():
    vgs_parts = ["module conf_floats;\n"]
    runs = []

    for fmt in ["f32", "f64"]:
        vals = float_special_bits(fmt)
        for op, fn in [("FADD", oracle_fadd), ("FSUB", oracle_fsub), ("FMUL", oracle_fmul), ("FDIV", oracle_fdiv), ("FMIN", oracle_fmin), ("FMAX", oracle_fmax)]:
            nname = f"{op.lower()}_{fmt}"
            vgs_parts.append(
                f"NODE @{nname} {{\n"
                f"  SIGNATURE: (a: {fmt}, b: {fmt}) -> ({fmt});\n"
                f"  EFFECTS: [];\n"
                f"  CONTRACT {{}}\n"
                f"  BODY {{\n"
                f"    %0 = {op}(a, b);\n"
                f"    RETURN(%0);\n"
                f"  }}\n"
                f"}}\n"
            )
            cases = []
            for a in vals:
                for b in vals:
                    res = fn(fmt, a, b)
                    cases.append({
                        "inputs": {"a": oracle_float_to_json(fmt, a), "b": oracle_float_to_json(fmt, b)},
                        "outputs": [oracle_float_to_json(fmt, res)],
                    })
            runs.append((nname, cases))

        # FMA
        nname = f"fma_{fmt}"
        vgs_parts.append(
            f"NODE @{nname} {{\n"
            f"  SIGNATURE: (a: {fmt}, b: {fmt}, c: {fmt}) -> ({fmt});\n"
            f"  EFFECTS: [];\n"
            f"  CONTRACT {{}}\n"
            f"  BODY {{\n"
            f"    %0 = FMA(a, b, c);\n"
            f"    RETURN(%0);\n"
            f"  }}\n"
            f"}}\n"
        )
        cases = []
        sub = vals[:10] + vals[-4:]
        for a in sub[:8]:
            for b in sub[:8]:
                for c in sub[:6]:
                    res = oracle_fma(fmt, a, b, c)
                    cases.append({
                        "inputs": {
                            "a": oracle_float_to_json(fmt, a),
                            "b": oracle_float_to_json(fmt, b),
                            "c": oracle_float_to_json(fmt, c),
                        },
                        "outputs": [oracle_float_to_json(fmt, res)],
                    })
        # Add FMA-specific single-rounding check: (1 + eps)*1 + tiny where intermediate > 2*mantissa
        if fmt == "f32":
            # 1.5 * (1 + 2^-23) - 2^-100: midpoint tie broken down by -2^-100
            a = 0x3FC00000
            b = 0x3F800001
            c = (-127 + 27)  # 2^-100: exp_field=27 => (1<<31)|(27<<23)
            c_neg = 0x80000000 | (27 << 23)
            for cx in [0, c_neg, 27 << 23]:
                res = oracle_fma("f32", a, b, cx)
                cases.append({
                    "inputs": {
                        "a": oracle_float_to_json("f32", a),
                        "b": oracle_float_to_json("f32", b),
                        "c": oracle_float_to_json("f32", cx),
                    },
                    "outputs": [oracle_float_to_json("f32", res)],
                })
        runs.append((nname, cases))

        # Unary: FNEG, FABS, FSQRT
        for op, fn in [("FNEG", oracle_fneg), ("FABS", oracle_fabs), ("FSQRT", oracle_fsqrt)]:
            nname = f"{op.lower()}_{fmt}"
            vgs_parts.append(
                f"NODE @{nname} {{\n"
                f"  SIGNATURE: (a: {fmt}) -> ({fmt});\n"
                f"  EFFECTS: [];\n"
                f"  CONTRACT {{}}\n"
                f"  BODY {{\n"
                f"    %0 = {op}(a);\n"
                f"    RETURN(%0);\n"
                f"  }}\n"
                f"}}\n"
            )
            cases = []
            for a in vals:
                res = fn(fmt, a)
                cases.append({
                    "inputs": {"a": oracle_float_to_json(fmt, a)},
                    "outputs": [oracle_float_to_json(fmt, res)],
                })
            runs.append((nname, cases))

        # FCLAMP
        nname = f"fclamp_{fmt}"
        vgs_parts.append(
            f"NODE @{nname} {{\n"
            f"  SIGNATURE: (x: {fmt}, lo: {fmt}, hi: {fmt}) -> ({fmt});\n"
            f"  EFFECTS: [];\n"
            f"  CONTRACT {{}}\n"
            f"  BODY {{\n"
            f"    %0 = FCLAMP(x, lo, hi);\n"
            f"    RETURN(%0);\n"
            f"  }}\n"
            f"}}\n"
        )
        cases = []
        e, m, cnan, sign_mask, _, _, _ = fmt_masks(fmt)
        one = ((1 << (e - 1)) - 1) << m
        neg_one = sign_mask | one
        for lo, hi in [(neg_one, one), (0, 0), (sign_mask, 0), (one, neg_one), (cnan, one), (neg_one, cnan)]:
            for x in vals:
                res = oracle_fclamp(fmt, x, lo, hi)
                entry = {
                    "inputs": {
                        "x": oracle_float_to_json(fmt, x),
                        "lo": oracle_float_to_json(fmt, lo),
                        "hi": oracle_float_to_json(fmt, hi),
                    }
                }
                if isinstance(res, str):
                    entry["trap"] = res
                else:
                    entry["outputs"] = [oracle_float_to_json(fmt, res)]
                cases.append(entry)
        runs.append((nname, cases))

        # FCOPYSIGN
        nname = f"fcopysign_{fmt}"
        vgs_parts.append(
            f"NODE @{nname} {{\n"
            f"  SIGNATURE: (x: {fmt}, y: {fmt}) -> ({fmt});\n"
            f"  EFFECTS: [];\n"
            f"  CONTRACT {{}}\n"
            f"  BODY {{\n"
            f"    %0 = FCOPYSIGN(x, y);\n"
            f"    RETURN(%0);\n"
            f"  }}\n"
            f"}}\n"
        )
        cases = []
        for x in vals:
            for y in [0, sign_mask, one, neg_one, f_pos_inf(fmt), f_neg_inf(fmt), cnan]:
                res = oracle_fcopysign(fmt, x, y)
                entry = {
                    "inputs": {
                        "x": oracle_float_to_json(fmt, x),
                        "y": oracle_float_to_json(fmt, y),
                    }
                }
                if isinstance(res, str):
                    entry["trap"] = res
                else:
                    entry["outputs"] = [oracle_float_to_json(fmt, res)]
                cases.append(entry)
        runs.append((nname, cases))

    write_vgs_and_vectors("floats", "\n".join(vgs_parts), runs)


def gen_narrow_float_arith():
    # Direct node oracle vectors for f16 and bf16 arithmetic ops (storage-only in VG-Sym per VG-T0105,
    # tested directly against Node structures in conformance.rs).
    data = {}
    for fmt in ["f16", "bf16"]:
        vals = float_special_bits(fmt)
        fmt_data = {}
        for op, fn in [("FADD", oracle_fadd), ("FSUB", oracle_fsub), ("FMUL", oracle_fmul), ("FDIV", oracle_fdiv), ("FMIN", oracle_fmin), ("FMAX", oracle_fmax)]:
            cases = []
            for a in vals:
                for b in vals:
                    cases.append([a, b, fn(fmt, a, b)])
            fmt_data[op] = cases
        for op, fn in [("FNEG", oracle_fneg), ("FABS", oracle_fabs), ("FSQRT", oracle_fsqrt)]:
            fmt_data[op] = [[a, fn(fmt, a)] for a in vals]
        # FMA
        fma_cases = []
        sub = vals[:8]
        for a in sub:
            for b in sub:
                for c in sub[:6]:
                    fma_cases.append([a, b, c, oracle_fma(fmt, a, b, c)])
        # Extra single-rounding FMA stress vectors
        if fmt == "bf16":
            for c in [0x0000, 0x8D80, 0x0D80]:
                fma_cases.append([0x3FC0, 0x3F81, c, oracle_fma("bf16", 0x3FC0, 0x3F81, c)])
                fma_cases.append([0x3F90, 0x3F68, c, oracle_fma("bf16", 0x3F90, 0x3F68, c)])
            fma_cases.append([0x5F80, 0x5F80, 0xFF00, oracle_fma("bf16", 0x5F80, 0x5F80, 0xFF00)])
        else:
            fma_cases.append([0x5C00, 0x5C00, 0xF800, oracle_fma("f16", 0x5C00, 0x5C00, 0xF800)])
            fma_cases.append([0x8000, 0x3C00, 0x8000, oracle_fma("f16", 0x8000, 0x3C00, 0x8000)])
        fmt_data["FMA"] = fma_cases
        # FCLAMP
        _, _, cnan, sign_mask, _, _, _ = fmt_masks(fmt)
        e, m, _ = FLOAT_FMTS[fmt]
        one = ((1 << (e - 1)) - 1) << m
        neg_one = sign_mask | one
        clamp_cases = []
        for lo, hi in [(neg_one, one), (sign_mask, 0), (one, neg_one), (cnan, one)]:
            for x in vals:
                res = oracle_fclamp(fmt, x, lo, hi)
                clamp_cases.append([x, lo, hi, res])
        fmt_data["FCLAMP"] = clamp_cases
        # FCOPYSIGN
        cs_cases = []
        for x in vals:
            for y in [0, sign_mask, one, neg_one, cnan]:
                cs_cases.append([x, y, oracle_fcopysign(fmt, x, y)])
        fmt_data["FCOPYSIGN"] = cs_cases
        data[fmt] = fmt_data

    (VEC_DIR / "narrow_float_arith.json").write_text(json.dumps(data, indent=2) + "\n")


def gen_compare_logic():
    vgs_parts = ["module conf_compare_logic;\n"]
    runs = []

    # Bool EQ, NE, BAND, BOR, BNOT
    for op in ["EQ", "NE", "BAND", "BOR"]:
        nname = f"{op.lower()}_bool"
        vgs_parts.append(
            f"NODE @{nname} {{\n"
            f"  SIGNATURE: (a: Bool, b: Bool) -> (Bool);\n"
            f"  EFFECTS: [];\n"
            f"  CONTRACT {{}}\n"
            f"  BODY {{\n"
            f"    %0 = {op}(a, b);\n"
            f"    RETURN(%0);\n"
            f"  }}\n"
            f"}}\n"
        )
        cases = []
        for a in [False, True]:
            for b in [False, True]:
                r = (a == b) if op == "EQ" else ((a != b) if op == "NE" else ((a and b) if op == "BAND" else (a or b)))
                cases.append({"inputs": {"a": a, "b": b}, "outputs": [r]})
        runs.append((nname, cases))

    vgs_parts.append(
        "NODE @bnot_bool {\n"
        "  SIGNATURE: (a: Bool) -> (Bool);\n"
        "  EFFECTS: [];\n"
        "  CONTRACT {}\n"
        "  BODY {\n"
        "    %0 = BNOT(a);\n"
        "    RETURN(%0);\n"
        "  }\n"
        "}\n"
    )
    runs.append(("bnot_bool", [
        {"inputs": {"a": False}, "outputs": [True]},
        {"inputs": {"a": True}, "outputs": [False]},
    ]))

    # Integer EQ, NE, LT/LE/GT/GE .{s, u}
    for ty_name, bits, signed in [("u8", 8, False), ("i8", 8, True), ("u64", 64, False), ("i64", 64, True)]:
        vals = int_test_values(bits, signed, seed=0xC049 + bits + (1 if signed else 0))[:10]
        mode = "s" if signed else "u"
        for op in ["EQ", "NE"]:
            nname = f"{op.lower()}_{ty_name}"
            vgs_parts.append(
                f"NODE @{nname} {{\n"
                f"  SIGNATURE: (a: {ty_name}, b: {ty_name}) -> (Bool);\n"
                f"  EFFECTS: [];\n"
                f"  CONTRACT {{}}\n"
                f"  BODY {{\n"
                f"    %0 = {op}(a, b);\n"
                f"    RETURN(%0);\n"
                f"  }}\n"
                f"}}\n"
            )
            cases = [
                {"inputs": {"a": json_val(ty_name, a), "b": json_val(ty_name, b)},
                 "outputs": [a == b if op == "EQ" else a != b]}
                for a in vals for b in vals
            ]
            runs.append((nname, cases))
        for op in ["LT", "LE", "GT", "GE"]:
            nname = f"{op.lower()}_{mode}_{ty_name}"
            vgs_parts.append(
                f"NODE @{nname} {{\n"
                f"  SIGNATURE: (a: {ty_name}, b: {ty_name}) -> (Bool);\n"
                f"  EFFECTS: [];\n"
                f"  CONTRACT {{}}\n"
                f"  BODY {{\n"
                f"    %0 = {op}.{mode}(a, b);\n"
                f"    RETURN(%0);\n"
                f"  }}\n"
                f"}}\n"
            )
            cases = []
            for a in vals:
                for b in vals:
                    r = (a < b) if op == "LT" else ((a <= b) if op == "LE" else ((a > b) if op == "GT" else (a >= b)))
                    cases.append({"inputs": {"a": json_val(ty_name, a), "b": json_val(ty_name, b)}, "outputs": [r]})
            runs.append((nname, cases))

    # Float EQ, NE, LT.f, LE.f, GT.f, GE.f
    for fmt in ["f32", "f64"]:
        vals = float_special_bits(fmt)[:14]
        for op in ["EQ", "NE", "LT", "LE", "GT", "GE"]:
            form = op if op in ("EQ", "NE") else f"{op}.f"
            nname = f"{op.lower()}_f_{fmt}"
            vgs_parts.append(
                f"NODE @{nname} {{\n"
                f"  SIGNATURE: (a: {fmt}, b: {fmt}) -> (Bool);\n"
                f"  EFFECTS: [];\n"
                f"  CONTRACT {{}}\n"
                f"  BODY {{\n"
                f"    %0 = {form}(a, b);\n"
                f"    RETURN(%0);\n"
                f"  }}\n"
                f"}}\n"
            )
            cases = [
                {"inputs": {"a": oracle_float_to_json(fmt, a), "b": oracle_float_to_json(fmt, b)},
                 "outputs": [oracle_fcmp(fmt, op, a, b)]}
                for a in vals for b in vals
            ]
            runs.append((nname, cases))

    write_vgs_and_vectors("compare_logic", "\n".join(vgs_parts), runs)


def gen_cast_and_bytes():
    vgs_parts = ["module conf_cast_bytes;\n"]
    runs = []

    # 1. Bool -> u8, i32
    for dst in ["u8", "i32"]:
        nname = f"cast_bool_{dst}"
        vgs_parts.append(
            f"NODE @{nname} {{\n"
            f"  SIGNATURE: (x: Bool) -> ({dst});\n"
            f"  EFFECTS: [];\n"
            f"  CONTRACT {{}}\n"
            f"  BODY {{\n"
            f"    %0 = CAST.{dst}(x);\n"
            f"    RETURN(%0);\n"
            f"  }}\n"
            f"}}\n"
        )
        runs.append((nname, [
            {"inputs": {"x": False}, "outputs": [0]},
            {"inputs": {"x": True}, "outputs": [1]},
        ]))

    # 2. Int -> Int (widening and narrowing / sign changes)
    int_cast_pairs = [("u8", "u16"), ("u8", "i16"), ("i16", "i8"), ("i32", "u32"), ("i64", "u8")]
    for src, dst in int_cast_pairs:
        sbits = int(src[1:])
        ssig = src.startswith("i")
        dbits = int(dst[1:])
        dsig = dst.startswith("i")
        dlo, dhi = int_bounds(dbits, dsig)
        svals = int_test_values(sbits, ssig, seed=0xCA57 + sbits + dbits)[:10]
        svals.extend([dlo, dhi])
        # clip svals to source range
        slo, shi = int_bounds(sbits, ssig)
        svals = sorted({max(slo, min(shi, v)) for v in svals})
        if dlo - 1 >= slo:
            svals.append(dlo - 1)
        if dhi + 1 <= shi:
            svals.append(dhi + 1)
        nname = f"cast_{src}_{dst}"
        vgs_parts.append(
            f"NODE @{nname} {{\n"
            f"  SIGNATURE: (x: {src}) -> ({dst});\n"
            f"  EFFECTS: [];\n"
            f"  CONTRACT {{}}\n"
            f"  BODY {{\n"
            f"    %0 = CAST.{dst}(x);\n"
            f"    RETURN(%0);\n"
            f"  }}\n"
            f"}}\n"
        )
        cases = []
        for x in svals:
            entry = {"inputs": {"x": json_val(src, x)}}
            if dlo <= x <= dhi:
                entry["outputs"] = [json_val(dst, x)]
            else:
                entry["trap"] = "CAST.in_range"
            cases.append(entry)
        runs.append((nname, cases))

    # 3. Int -> Float (including u64 -> f16 overflow and u64 -> bf16 single rounding!)
    for src, dst in [("u64", "f16"), ("u64", "bf16"), ("i64", "f32"), ("u64", "f64")]:
        sbits = int(src[1:])
        ssig = src.startswith("i")
        svals = int_test_values(sbits, ssig, seed=0x1F10 + sbits)
        if src == "u64" and dst == "bf16":
            svals.extend([(1 << 63) + (1 << 55), (1 << 63) + (1 << 55) + 1])
        if dst == "f16":
            svals.extend([65504, 65519, 65520])
        nname = f"cast_{src}_{dst}"
        vgs_parts.append(
            f"NODE @{nname} {{\n"
            f"  SIGNATURE: (x: {src}) -> ({dst});\n"
            f"  EFFECTS: [];\n"
            f"  CONTRACT {{}}\n"
            f"  BODY {{\n"
            f"    %0 = CAST.{dst}(x);\n"
            f"    RETURN(%0);\n"
            f"  }}\n"
            f"}}\n"
        )
        cases = []
        for x in svals:
            res = fraction_to_float(dst, x < 0, Fraction(abs(x)))
            cases.append({"inputs": {"x": json_val(src, x)}, "outputs": [oracle_float_to_json(dst, res)]})
        runs.append((nname, cases))

    # 4. Float -> Int (truncation toward zero and CAST.in_range)
    for src, dst in [("f32", "u8"), ("f32", "i32"), ("f64", "u64"), ("f64", "i64"), ("f16", "i16"), ("bf16", "u32")]:
        dbits = int(dst[1:])
        dsig = dst.startswith("i")
        dlo, dhi = int_bounds(dbits, dsig)
        fvals = float_special_bits(src)
        # Add fractional values (-0.9, 0.9, 255.9, etc.)
        for extra in [Fraction(-9, 10), Fraction(9, 10), Fraction(2559, 10), Fraction(256), Fraction(-1)]:
            fvals.append(fraction_to_float(src, extra < 0, abs(extra)))
        nname = f"cast_{src}_{dst}"
        vgs_parts.append(
            f"NODE @{nname} {{\n"
            f"  SIGNATURE: (x: {src}) -> ({dst});\n"
            f"  EFFECTS: [];\n"
            f"  CONTRACT {{}}\n"
            f"  BODY {{\n"
            f"    %0 = CAST.{dst}(x);\n"
            f"    RETURN(%0);\n"
            f"  }}\n"
            f"}}\n"
        )
        cases = []
        for x in dict.fromkeys(fvals):
            entry = {"inputs": {"x": oracle_float_to_json(src, x)}}
            if f_is_nan(src, x) or f_is_inf(src, x):
                entry["trap"] = "CAST.in_range"
            else:
                _, fx = f_to_fraction(src, x)
                # truncate toward zero
                trunc = int(fx)  # int(Fraction) in Python truncates toward zero!
                if dlo <= trunc <= dhi:
                    entry["outputs"] = [json_val(dst, trunc)]
                else:
                    entry["trap"] = "CAST.in_range"
            cases.append(entry)
        runs.append((nname, cases))

    # 5. Float -> Float (widening and narrowing)
    for src, dst in [("f16", "f32"), ("f32", "f16"), ("bf16", "f32"), ("f32", "bf16"), ("f32", "f64"), ("f64", "f32")]:
        fvals = float_special_bits(src)
        nname = f"cast_{src}_{dst}"
        vgs_parts.append(
            f"NODE @{nname} {{\n"
            f"  SIGNATURE: (x: {src}) -> ({dst});\n"
            f"  EFFECTS: [];\n"
            f"  CONTRACT {{}}\n"
            f"  BODY {{\n"
            f"    %0 = CAST.{dst}(x);\n"
            f"    RETURN(%0);\n"
            f"  }}\n"
            f"}}\n"
        )
        cases = []
        for x in fvals:
            if f_is_nan(src, x):
                res = FLOAT_FMTS[dst][2]
            elif f_is_inf(src, x):
                res = f_neg_inf(dst) if f_is_neg(src, x) else f_pos_inf(dst)
            elif f_is_zero(src, x):
                res = f_zero(dst, f_is_neg(src, x))
            else:
                neg, fx = f_to_fraction(src, x)
                res = fraction_to_float(dst, neg, abs(fx))
            cases.append({"inputs": {"x": oracle_float_to_json(src, x)}, "outputs": [oracle_float_to_json(dst, res)]})
        runs.append((nname, cases))

    # 6. AS_BYTES (Bool, i16, f32 incl. NaN & -0.0, f16, bf16, Tensor<u16, [2, 2]>)
    vgs_parts.append(
        "NODE @as_bytes_bool { SIGNATURE: (x: Bool) -> (Slice<u8>); EFFECTS: []; CONTRACT {} BODY { %0 = AS_BYTES(x); RETURN(%0); } }\n"
        "NODE @as_bytes_i16 { SIGNATURE: (x: i16) -> (Slice<u8>); EFFECTS: []; CONTRACT {} BODY { %0 = AS_BYTES(x); RETURN(%0); } }\n"
        "NODE @as_bytes_f32 { SIGNATURE: (x: f32) -> (Slice<u8>); EFFECTS: []; CONTRACT {} BODY { %0 = AS_BYTES(x); RETURN(%0); } }\n"
        "NODE @as_bytes_f16 { SIGNATURE: (x: f16) -> (Slice<u8>); EFFECTS: []; CONTRACT {} BODY { %0 = AS_BYTES(x); RETURN(%0); } }\n"
        "NODE @as_bytes_bf16 { SIGNATURE: (x: bf16) -> (Slice<u8>); EFFECTS: []; CONTRACT {} BODY { %0 = AS_BYTES(x); RETURN(%0); } }\n"
        "NODE @as_bytes_tensor { SIGNATURE: (t: Tensor<u16, [2, 2]>) -> (Slice<u8>); EFFECTS: []; CONTRACT {} BODY { %0 = AS_BYTES(t); RETURN(%0); } }\n"
    )
    runs.append(("as_bytes_bool", [
        {"inputs": {"x": False}, "outputs": [[0]]},
        {"inputs": {"x": True}, "outputs": [[1]]},
    ]))
    runs.append(("as_bytes_i16", [
        {"inputs": {"x": -258}, "outputs": [list((-258).to_bytes(2, "little", signed=True))]},
        {"inputs": {"x": 32767}, "outputs": [list((32767).to_bytes(2, "little", signed=True))]},
    ]))
    runs.append(("as_bytes_f32", [
        {"inputs": {"x": oracle_float_to_json("f32", b)}, "outputs": [list(b.to_bytes(4, "little"))]}
        for b in [0, 0x80000000, 0x7FC00000, 0x3F800000]
    ]))
    runs.append(("as_bytes_f16", [
        {"inputs": {"x": oracle_float_to_json("f16", b)}, "outputs": [list(b.to_bytes(2, "little"))]}
        for b in [0, 0x8000, 0x7E00, 0x3C00]
    ]))
    runs.append(("as_bytes_bf16", [
        {"inputs": {"x": oracle_float_to_json("bf16", b)}, "outputs": [list(b.to_bytes(2, "little"))]}
        for b in [0, 0x8000, 0x7FC0, 0x3F80]
    ]))
    runs.append(("as_bytes_tensor", [
        {"inputs": {"t": [[1, 256], [513, 65535]]}, "outputs": [[1, 0, 0, 1, 1, 2, 255, 255]]},
    ]))

    write_vgs_and_vectors("cast_and_bytes", "\n".join(vgs_parts), runs)


def gen_structure_and_collections():
    vgs = """module conf_struct_coll;

NODE @tuple_proj {
  SIGNATURE: (a: i32, b: Bool, c: f32) -> (i32, Bool, f32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = TUPLE(a, b, c);
    %1 = PROJ(%0, 0:u32);
    %2 = PROJ(%0, 1:u32);
    %3 = PROJ(%0, 2:u32);
    RETURN(%1, %2, %3);
  }
}

NODE @select_strict {
  SIGNATURE: (c: Bool, a: i32, b: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = DIV.checked(100:i32, b);
    %1 = SELECT(c, a, %0);
    RETURN(%1);
  }
}

NODE @result_ops {
  SIGNATURE: (make_ok: Bool, val: u32, err: u16) -> (Bool, u32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0: Result<u32, u16> = OK(val);
    %1: Result<u32, u16> = ERR(err);
    %2 = SELECT(make_ok, %0, %1);
    %3 = IS_OK(%2);
    %4 = GET_OK(%2);
    RETURN(%3, %4);
  }
}

NODE @result_err_ops {
  SIGNATURE: (make_ok: Bool, val: u32, err: u16) -> (u16);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0: Result<u32, u16> = OK(val);
    %1: Result<u32, u16> = ERR(err);
    %2 = SELECT(make_ok, %0, %1);
    %3 = GET_ERR(%2);
    RETURN(%3);
  }
}

NODE @collections_arr {
  SIGNATURE: (xs: Array<u32, 4>, i: u64, v: u32, lo: u64, hi: u64) -> (u32, Array<u32, 4>, u64, Slice<u32>, Array<u32, 8>);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = INDEX(xs, i);
    %1 = UPDATE(xs, i, v);
    %2 = LEN(xs);
    %3 = SLICE(xs, lo, hi);
    %4 = CONCAT(xs, %1);
    RETURN(%0, %1, %2, %3, %4);
  }
}

NODE @collections_slice {
  SIGNATURE: (xs: Slice<u32>, ys: Slice<u32>, i: u64, lo: u64, hi: u64) -> (u32, Slice<u32>, u64, Slice<u32>, Slice<u32>);
  EFFECTS: [ARENA];
  CONTRACT {}
  BODY {
    %0 = INDEX(xs, i);
    %1 = UPDATE(xs, i, 99:u32);
    %2 = LEN(xs);
    %3 = SLICE(xs, lo, hi);
    %4 = CONCAT(xs, ys);
    RETURN(%0, %1, %2, %3, %4);
  }
}
"""
    runs = [
        ("tuple_proj", [
            {"inputs": {"a": -42, "b": True, "c": "-0.0"}, "outputs": [-42, True, "-0.0"]},
        ]),
        ("select_strict", [
            {"inputs": {"c": True, "a": 7, "b": 4}, "outputs": [7]},
            {"inputs": {"c": False, "a": 7, "b": 4}, "outputs": [25]},
            # Strict evaluation: b=0 traps on DIV even when c=true!
            {"inputs": {"c": True, "a": 7, "b": 0}, "trap": "DIV.nonzero"},
        ]),
        ("result_ops", [
            {"inputs": {"make_ok": True, "val": 123, "err": 9}, "outputs": [True, 123]},
            {"inputs": {"make_ok": False, "val": 123, "err": 9}, "trap": "GET_OK.is_ok"},
        ]),
        ("result_err_ops", [
            {"inputs": {"make_ok": False, "val": 123, "err": 9}, "outputs": [9]},
            {"inputs": {"make_ok": True, "val": 123, "err": 9}, "trap": "GET_ERR.is_err"},
        ]),
        ("collections_arr", [
            {
                "inputs": {"xs": [10, 20, 30, 40], "i": "2", "v": 77, "lo": "1", "hi": "3"},
                "outputs": [30, [10, 20, 77, 40], "4", [20, 30], [10, 20, 30, 40, 10, 20, 77, 40]],
            },
            {"inputs": {"xs": [1, 2, 3, 4], "i": "4", "v": 0, "lo": "0", "hi": "1"}, "trap": "INDEX.in_bounds"},
            {"inputs": {"xs": [1, 2, 3, 4], "i": "0", "v": 0, "lo": "3", "hi": "2"}, "trap": "SLICE.in_bounds"},
            {"inputs": {"xs": [1, 2, 3, 4], "i": "0", "v": 0, "lo": "0", "hi": "5"}, "trap": "SLICE.in_bounds"},
        ]),
        ("collections_slice", [
            {
                "inputs": {"xs": [5, 6, 7], "ys": [8], "i": "0", "lo": "1", "hi": "1"},
                "outputs": [5, [99, 6, 7], "3", [], [5, 6, 7, 8]],
            },
            {"inputs": {"xs": [], "ys": [], "i": "0", "lo": "0", "hi": "0"}, "trap": "INDEX.in_bounds"},
        ]),
    ]
    write_vgs_and_vectors("struct_coll", vgs, runs)


def gen_combinators_and_calls():
    vgs = """module conf_combinators;

NODE @add_k {
  SIGNATURE: (x: i32, k: i32) -> (i32);
  EFFECTS: [];
  CONTRACT { PRE: x >= -1000; }
  BODY {
    %0 = ADD.checked(x, k);
    RETURN(%0);
  }
}

NODE @map_arr {
  SIGNATURE: (xs: Array<i32, 3>, k: i32) -> (Array<i32, 3>);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = SIMD_MAP(@add_k, xs, k);
    RETURN(%0);
  }
}

NODE @map_tensor {
  SIGNATURE: (t: Tensor<i32, [2, 2]>, k: i32) -> (Tensor<i32, [2, 2]>);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = SIMD_MAP(@add_k, t, k);
    RETURN(%0);
  }
}

NODE @fold_step {
  SIGNATURE: (acc: i32, elem: i32, mult: i32) -> (i32);
  EFFECTS: [];
  CONTRACT { PRE: elem >= 0; }
  BODY {
    // Non-commutative step: acc * mult - elem to verify strict left-to-right element order.
    %0 = MUL.checked(acc, mult);
    %1 = SUB.checked(%0, elem);
    RETURN(%1);
  }
}

NODE @fold_arr {
  SIGNATURE: (xs: Slice<i32>, mult: i32, init: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {
    INVARIANT @inv(k, acc): k >= 0;
  }
  BODY {
    %0 = FOLD_BOUNDED(@fold_step, xs, mult, init=init, invariant=@inv);
    RETURN(%0);
  }
}

NODE @gen_kernel {
  SIGNATURE: (idx: u64, scale: u64) -> (u64);
  EFFECTS: [];
  CONTRACT { PRE: scale <= 1000; }
  BODY {
    %0 = MUL.checked(idx, scale);
    RETURN(%0);
  }
}

NODE @gen_arr {
  SIGNATURE: (scale: u64) -> (Array<u64, 4>);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = GENERATE(@gen_kernel, 4:u64, scale);
    RETURN(%0);
  }
}

NODE @gen_slice {
  SIGNATURE: (n: u64, scale: u64) -> (Slice<u64>);
  EFFECTS: [ARENA];
  CONTRACT {}
  BODY {
    %0 = GENERATE(@gen_kernel, n, scale);
    RETURN(%0);
  }
}

NODE @safe_div {
  SIGNATURE: (a: i32, b: i32) -> (i32, i32);
  EFFECTS: [];
  CONTRACT { PRE: b != 0; }
  BODY {
    %0 = DIV.checked(a, b);
    %1 = MOD.checked(a, b);
    RETURN(%0, %1);
  }
}

NODE @zero_pair {
  SIGNATURE: (a: i32, b: i32) -> (i32, i32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    RETURN(0:i32, a);
  }
}

NODE @guard_demo {
  SIGNATURE: (a: i32, b: i32) -> (i32, i32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = NE(b, 0:i32);
    %1, %2 = ASSERT_GUARD(%0, @safe_div, a, b, else=@zero_pair);
    RETURN(%1, %2);
  }
}

NODE @guard_branch_demo {
  SIGNATURE: (a: i32, b: i32) -> (i32, i32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = NE(b, 0:i32);
    %1, %2 = ASSERT_GUARD.branch(%0, @safe_div, a, b, else=@zero_pair);
    RETURN(%1, %2);
  }
}

NODE @call_demo {
  SIGNATURE: (a: i32, b: i32) -> (i32, i32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0, %1 = CALL_HASH(@safe_div, a, b);
    RETURN(%0, %1);
  }
}
"""
    runs = [
        ("map_arr", [
            {"inputs": {"xs": [1, 2, 3], "k": 10}, "outputs": [[11, 12, 13]]},
            {"inputs": {"xs": [1, -1001, 3], "k": 10}, "trap": "MAP.pre"},
        ]),
        ("map_tensor", [
            {"inputs": {"t": [[1, 2], [3, 4]], "k": -5}, "outputs": [[[-4, -3], [-2, -1]]]},
        ]),
        ("fold_arr", [
            # Step 0: 1*2 - 3 = -1; Step 1: (-1)*2 - 4 = -6; Step 2: (-6)*2 - 5 = -17
            {"inputs": {"xs": [3, 4, 5], "mult": 2, "init": 1}, "outputs": [-17]},
            {"inputs": {"xs": [], "mult": 2, "init": 42}, "outputs": [42]},
            {"inputs": {"xs": [3, -1], "mult": 2, "init": 1}, "trap": "FOLD.pre"},
        ]),
        ("gen_arr", [
            {"inputs": {"scale": "10"}, "outputs": [["0", "10", "20", "30"]]},
            {"inputs": {"scale": "1001"}, "trap": "GEN.pre"},
        ]),
        ("gen_slice", [
            {"inputs": {"n": "0", "scale": "5"}, "outputs": [[]]},
            {"inputs": {"n": "3", "scale": "7"}, "outputs": [["0", "7", "14"]]},
        ]),
        ("guard_demo", [
            {"inputs": {"a": 17, "b": 5}, "outputs": [3, 2]},
            {"inputs": {"a": 17, "b": 0}, "outputs": [0, 17]},
        ]),
        ("guard_branch_demo", [
            {"inputs": {"a": -17, "b": 5}, "outputs": [-3, -2]},
            {"inputs": {"a": -17, "b": 0}, "outputs": [0, -17]},
        ]),
        ("call_demo", [
            {"inputs": {"a": 19, "b": -4}, "outputs": [-4, 3]},
            {"inputs": {"a": 19, "b": 0}, "trap": "CALL.pre"},
        ]),
    ]
    write_vgs_and_vectors("combinators", vgs, runs)


def gen_effects():
    vgs = """module conf_effects;

NODE @effects_pipeline {
  SIGNATURE: (e: Eff, clk: Clock, fin: FileIn, fout: FileOut, max_bytes: u64) -> (Eff, u64, Result<Slice<u8>, u32>, Result<u64, u32>);
  EFFECTS: [ARENA, CLOCK, FILE_READ, FILE_WRITE];
  CONTRACT {}
  BODY {
    %0, %1 = CLOCK_READ(e, clk);
    %2, %3 = FILE_READ(%0, fin, max_bytes);
    %4 = IS_OK(%3);
    %5 = SELECT(%4, [65, 66]:Array<u8, 2>, [67, 68]:Array<u8, 2>);
    %6 = SLICE(%5, 0:u64, 2:u64);
    %7, %8 = FILE_WRITE(%2, fout, %6);
    %9, %10 = CLOCK_READ(%7, clk);
    %11 = SUB.checked(%10, %1);
    RETURN(%9, %11, %3, %8);
  }
}
"""
    runs = [
        ("effects_pipeline", [
            {
                "inputs": {
                    "clk": {"handle": "Clock", "id": 0},
                    "fin": {"handle": "FileIn", "id": 1},
                    "fout": {"handle": "FileOut", "id": 2},
                    "max_bytes": "16",
                },
                "trace": [
                    {"op": "CLOCK_READ", "result": "1000"},
                    {"op": "FILE_READ", "result": {"ok": [104, 105]}},
                    {"op": "FILE_WRITE", "result": {"ok": "2"}},
                    {"op": "CLOCK_READ", "result": "1250"},
                ],
                "outputs": ["250", {"ok": [104, 105]}, {"ok": "2"}],
            },
            {
                "inputs": {
                    "clk": {"handle": "Clock", "id": 0},
                    "fin": {"handle": "FileIn", "id": 1},
                    "fout": {"handle": "FileOut", "id": 2},
                    "max_bytes": "1",
                },
                "trace": [
                    {"op": "CLOCK_READ", "result": "500"},
                    {"op": "FILE_READ", "result": {"err": 2}},
                    {"op": "FILE_WRITE", "result": {"err": 1}},
                    {"op": "CLOCK_READ", "result": "500"},
                ],
                "outputs": ["0", {"err": 2}, {"err": 1}],
            },
            {
                "inputs": {
                    "clk": {"handle": "Clock", "id": 0},
                    "fin": {"handle": "FileIn", "id": 1},
                    "fout": {"handle": "FileOut", "id": 2},
                    "max_bytes": "16",
                },
                "trace": [
                    {"op": "CLOCK_READ", "result": "500"},
                    {"op": "FILE_READ", "result": {"ok": [1]}},
                    {"op": "FILE_WRITE", "result": {"ok": "2"}},
                    {"op": "CLOCK_READ", "result": "499"},
                ],
                "trap": "non-monotonic",
            },
            {
                "inputs": {
                    "clk": {"handle": "Clock", "id": 0},
                    "fin": {"handle": "FileIn", "id": 1},
                    "fout": {"handle": "FileOut", "id": 2},
                    "max_bytes": "16",
                },
                "trace": [
                    {"op": "FILE_READ", "result": {"ok": [1]}},
                ],
                "trap": "effect trace mismatch",
            },
        ]),
    ]
    write_vgs_and_vectors("effects", vgs, runs)


def gen_inline_run_example():
    # Also include a small .vgs file using the inline `// run: @node {...} => [...]` format
    # from docs/16 line 22 so both `file:` and inline `// run:` syntax are exercised.
    vgs = """// run: @add_inline {"a": 120, "b": 10} => [127]
// run: @add_inline {"a": -100, "b": -50} => [-128]
module conf_inline;

NODE @add_inline {
  SIGNATURE: (a: i8, b: i8) -> (i8);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = ADD.sat(a, b);
    RETURN(%0);
  }
}
"""
    (OUT_DIR / "inline_sat.vgs").write_text(vgs)


def main():
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    VEC_DIR.mkdir(parents=True, exist_ok=True)
    gen_int_arith()
    gen_bits()
    gen_floats()
    gen_narrow_float_arith()
    gen_compare_logic()
    gen_cast_and_bytes()
    gen_structure_and_collections()
    gen_combinators_and_calls()
    gen_effects()
    gen_inline_run_example()
    print(f"Generated conformance files in {OUT_DIR}")


if __name__ == "__main__":
    main()
