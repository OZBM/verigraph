"""OmniLift Python front end (R-13): a typed, pure, int-only subset of Python to C.

Usage: python3 -I pylift.py SOURCE.py FUNCTION OUT.c
Exit 0 on success. Exit 2 with "pylift: LINE:COL: MESSAGE" on stderr when the function, or a
function or module constant it uses, is outside the subset.

Contract of the C it writes: for arguments of the annotated types whose ints fit in int64,
the C entry function returns what CPython returns, or reaches C undefined behavior exactly
when CPython raises or a value leaves int64. OmniLift cures that UB into Result::Err, so the
lift returns CPython's value or Err, never a different value.

Every operation is UB-free C that sets an error bit where CPython would raise or leave
int64 (arithmetic wraps in uint64_t and checks; division uses a divisor that cannot be 0).
The one UB is at the entry's exit: py_fail(e, v) is v + 1 / (1 - e) - 1, a division by zero,
immediate UB in LLVM, when e is 1. py_fail is noinline and the C is compiled at -O0 (no optnone), so no LLVM fold
can reason the division away; an nsw overflow would only be poison, which folds can drop.

Every C value is an int64_t; bool values are 0 or 1, and a bool argument that is neither
is Err.

Types: int (int64), bool, bytes (read-only, elements 0..255), list[int] or tuple[int, ...]
(read-only, int64 elements). An unannotated parameter is an int; an unannotated return is
bool when every return value is bool, else int.
"""

import ast
import sys

INT64_MIN, INT64_MAX = -(2**63), 2**63 - 1
INT, BOOL, BYTES, INTS = "int", "bool", "bytes", "ints"
SEQ = (BYTES, INTS)

C_RESERVED = set(
    """auto break case char const continue default do double else enum extern float for goto
    if inline int long register restrict return short signed sizeof static struct switch
    typedef union unsigned void volatile while bool true false main""".split()
)
RESERVED_PREFIXES = ("py_", "v_", "g_", "INT64_")

PRELUDE = r"""#include <stddef.h>
#include <stdint.h>

/* UB-free helpers: each returns the int64 result CPython computes and ORs 1 into *e where
   CPython raises or the result leaves int64. umax/umin are llvm.umax/llvm.umin, which
   OmniLift's interval analysis reads, so it proves divisors nonzero and table indices in
   bounds. py_sel(c, x, y) is c ? x : y for c in {0, 1}, with no branch. */
typedef int64_t i64;
typedef uint64_t u64;
#define PY_INLINE static inline __attribute__((always_inline))
PY_INLINE u64 py_umax(u64 a, u64 b) { return __builtin_elementwise_max(a, b); }
PY_INLINE u64 py_umin(u64 a, u64 b) { return __builtin_elementwise_min(a, b); }
PY_INLINE i64 py_sel(i64 c, i64 x, i64 y) { return y ^ ((x ^ y) & (i64)(0 - (u64)c)); }
PY_INLINE u64 py_uabs(i64 a) { u64 m = (u64)(a >> 63); return ((u64)a ^ m) - m; }
PY_INLINE i64 py_add(i64 a, i64 b, i64 *e) {
    i64 r = (i64)((u64)a + (u64)b);
    *e |= ((a ^ r) & (b ^ r)) < 0;
    return r;
}
PY_INLINE i64 py_sub(i64 a, i64 b, i64 *e) {
    i64 r = (i64)((u64)a - (u64)b);
    *e |= ((a ^ b) & (a ^ r)) < 0;
    return r;
}
PY_INLINE i64 py_neg(i64 a, i64 *e) { return py_sub(0, a, e); }
PY_INLINE i64 py_abs(i64 a, i64 *e) { return py_sel(a < 0, py_neg(a, e), a); }
PY_INLINE i64 py_mul(i64 a, i64 b, i64 *e) {
    /* High 64 bits of |a| * |b| from 32-bit limbs: no division, no 128-bit type. */
    u64 x = py_uabs(a), y = py_uabs(b);
    u64 x0 = x & 0xffffffffu, x1 = x >> 32, y0 = y & 0xffffffffu, y1 = y >> 32;
    u64 t = x1 * y0 + ((x0 * y0) >> 32);
    u64 w = (t & 0xffffffffu) + x0 * y1;
    u64 hi = x1 * y1 + (t >> 32) + (w >> 32);
    u64 mag = x * y;
    u64 limit = (u64)INT64_MAX + (u64)((a < 0) != (b < 0)); /* 2^63 when negative */
    *e |= (hi != 0) | (mag > limit);
    return (i64)((u64)a * (u64)b);
}
PY_INLINE i64 py_floordiv(i64 a, i64 b, i64 *e) {
    u64 x = py_uabs(a), y = py_uabs(b), d = py_umax(y, 1); /* b == 0 raises */
    u64 q = x / d, r = x % d;
    i64 neg = (a < 0) != (b < 0);
    u64 mag = q + (u64)(neg & (r != 0)); /* round toward -inf */
    *e |= (b == 0) | ((!neg) & (mag > (u64)INT64_MAX)); /* INT64_MIN // -1 */
    return py_sel(neg, (i64)(0 - mag), (i64)mag);
}
PY_INLINE i64 py_mod(i64 a, i64 b, i64 *e) {
    u64 x = py_uabs(a), y = py_uabs(b), d = py_umax(y, 1);
    u64 r = x % d;
    i64 neg = (a < 0) != (b < 0);
    u64 m = (u64)py_sel(neg & (r != 0), (i64)(y - r), (i64)r); /* takes the divisor's sign */
    *e |= b == 0;
    return py_sel(b < 0, (i64)(0 - m), (i64)m);
}
PY_INLINE i64 py_lshift(i64 a, i64 b, i64 *e) {
    i64 s = b & 63;
    i64 r = (i64)((u64)a << s);
    /* negative count: ValueError; bits shifted out (any a != 0 when b > 63): leaves int64 */
    *e |= (b < 0) | ((b > 63) & (a != 0)) | ((r >> s) != a);
    return py_sel(b > 63, 0, r);
}
PY_INLINE i64 py_rshift(i64 a, i64 b, i64 *e) {
    *e |= b < 0;
    return a >> (py_sel(b > 63, 63, b) & 63); /* a >> 63 is a >> b for every b > 63 */
}
PY_INLINE i64 py_pow(i64 base, i64 n, i64 *e) {
    i64 r = 1, err = n < 0; /* a negative exponent gives a float */
    for (int i = 0; i < 64; i++) {
        r = py_sel(n & 1, py_mul(r, base, &err), r);
        n = n >> 1;
        i64 sq_err = 0, sq = py_mul(base, base, &sq_err);
        err |= sq_err & (n != 0); /* only a square the result still needs can overflow it */
        base = sq;
    }
    *e |= err;
    return r;
}
PY_INLINE i64 py_min(i64 a, i64 b) { return py_sel(b < a, b, a); }
PY_INLINE i64 py_max(i64 a, i64 b) { return py_sel(b > a, b, a); }
PY_INLINE i64 py_bit_length(i64 a) {
    u64 u = py_uabs(a);
    i64 n = 0;
    for (int i = 0; i < 64; i++) n = py_sel((u >> i) != 0, i + 1, n);
    return n;
}
/* Python indexing: negative indices count from the end; out of range raises IndexError. */
PY_INLINE u64 py_index(i64 i, size_t n, i64 *e) {
    u64 k = (u64)i + ((u64)n & (u64)(i >> 63));
    *e |= k >= (u64)n;
    return k;
}
/* The read is clamped instead of branched on: an out-of-range index has already set *e, so
   the value read is never used, and a branch-free read keeps loop bodies straight. */
PY_INLINE u64 py_clamp(u64 k, size_t n) { return py_umin(k, py_umax((u64)n, 1) - 1); }
PY_INLINE i64 py_load_bytes(const uint8_t *xs, size_t n, i64 i, i64 *e) {
    return (i64)xs[py_clamp(py_index(i, n, e), n)];
}
PY_INLINE i64 py_load_ints(const i64 *xs, size_t n, i64 i, i64 *e) {
    return xs[py_clamp(py_index(i, n, e), n)];
}
PY_INLINE i64 py_load_table(const i64 *t, size_t n, i64 i, i64 *e) {
    return t[py_clamp(py_index(i, n, e), n)];
}
/* Elements of range(start, stop, step) for a constant nonzero step. */
PY_INLINE u64 py_range_len(i64 start, i64 stop, i64 step) {
    if (step > 0) return stop > start ? ((u64)stop - (u64)start - 1) / (u64)step + 1 : 0;
    return start > stop ? ((u64)start - (u64)stop - 1) / (0 - (u64)step) + 1 : 0;
}
/* The one UB: v when e is 0, division by zero when e is 1. A 32-bit division keeps the SMT
   query small. Not inlined, so no fold sees a constant e or reasons about the divisor before
   OmniLift lowers it. */
static __attribute__((noinline)) i64 py_fail(i64 e, i64 v) {
    uint32_t d = 1u - (uint32_t)e;
    return (i64)((u64)v + (u64)(1u / d) - 1);
}
"""


class Reject(Exception):
    def __init__(self, node, msg):
        super().__init__(msg)
        self.node = node
        self.msg = msg


def is_name(node, name):
    return isinstance(node, ast.Name) and node.id == name


def bound_names(node):
    """Names a node binds besides function definitions: every form, not only `x = ...`."""
    if isinstance(node, ast.Name) and isinstance(node.ctx, (ast.Store, ast.Del)):
        return [node.id]
    if isinstance(node, (ast.Import, ast.ImportFrom)):
        return [(a.asname or a.name).split(".")[0] for a in node.names if a.name != "*"]
    if isinstance(node, ast.ClassDef):
        return [node.name]
    if isinstance(node, ast.ExceptHandler) and node.name:
        return [node.name]
    if isinstance(node, (ast.MatchAs, ast.MatchStar)) and node.name:
        return [node.name]
    if isinstance(node, ast.MatchMapping) and node.rest:
        return [node.rest]
    return []


def const_int(node):
    """The int value of a constant int expression (literals, unary and binary operators)."""
    def ev(n):
        if isinstance(n, ast.Constant) and type(n.value) in (int, bool):
            return int(n.value)
        if isinstance(n, ast.UnaryOp) and isinstance(n.op, (ast.USub, ast.UAdd, ast.Invert)):
            v = ev(n.operand)
            return {ast.USub: -v, ast.UAdd: v, ast.Invert: ~v}[type(n.op)]
        if isinstance(n, ast.BinOp):
            a, b = ev(n.left), ev(n.right)
            ops = {
                ast.Add: lambda: a + b, ast.Sub: lambda: a - b, ast.Mult: lambda: a * b,
                ast.FloorDiv: lambda: a // b, ast.Mod: lambda: a % b,
                ast.BitAnd: lambda: a & b, ast.BitOr: lambda: a | b, ast.BitXor: lambda: a ^ b,
                ast.LShift: lambda: a << b if 0 <= b <= 128 else None,
                ast.RShift: lambda: a >> b if b >= 0 else None,
                ast.Pow: lambda: a**b if 0 <= b <= 128 else None,
            }
            f = ops.get(type(n.op))
            if f is None or (isinstance(n.op, (ast.FloorDiv, ast.Mod)) and b == 0):
                raise Reject(n, "not a constant int expression")
            v = f()
            if v is None:
                raise Reject(n, "constant shift or power out of range")
            return v
        raise Reject(n, "not a constant int expression")

    v = ev(node)
    if not INT64_MIN <= v <= INT64_MAX:
        raise Reject(node, f"constant {v} does not fit in int64")
    return v


def c_int(v):
    return "(-INT64_MAX - 1)" if v == INT64_MIN else f"INT64_C({v})"


def annotation_type(node, what):
    if node is None or is_name(node, "int"):
        return INT
    if is_name(node, "bool"):
        return BOOL
    if is_name(node, "bytes"):
        return BYTES
    if isinstance(node, ast.Subscript):
        base, arg = node.value, node.slice
        if (is_name(base, "list") or is_name(base, "Sequence")) and is_name(arg, "int"):
            return INTS
        if (
            is_name(base, "tuple")
            and isinstance(arg, ast.Tuple)
            and len(arg.elts) == 2
            and is_name(arg.elts[0], "int")
            and isinstance(arg.elts[1], ast.Constant)
            and arg.elts[1].value is Ellipsis
        ):
            return INTS
    raise Reject(node, f"unsupported {what} type annotation {ast.unparse(node)}")


class ModuleInfo:
    """Top-level functions, and module constants that nothing in the module can change."""

    def __init__(self, tree):
        self.functions = {}
        self.consts = {}  # name -> int
        self.tables = {}  # name -> list of int
        assigned = {}
        for stmt in tree.body:
            if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef)):
                self.functions[stmt.name] = stmt
            targets = []
            if isinstance(stmt, ast.Assign) and len(stmt.targets) == 1:
                targets = [(stmt.targets[0], stmt.value)]
            elif isinstance(stmt, ast.AnnAssign) and stmt.value is not None:
                targets = [(stmt.target, stmt.value)]
            for t, v in targets:
                if isinstance(t, ast.Name):
                    assigned.setdefault(t.id, []).append(v)
        mutated = set()
        for node in ast.walk(tree):
            if isinstance(node, (ast.Global, ast.Nonlocal)):
                mutated.update(node.names)
            elif isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
                mutated.add(node.value.id)  # a method call could mutate the object
            elif (
                isinstance(node, ast.Subscript)
                and isinstance(node.ctx, (ast.Store, ast.Del))
                and isinstance(node.value, ast.Name)
            ):
                mutated.add(node.value.id)
            elif isinstance(node, ast.AugAssign) and isinstance(node.target, ast.Name):
                mutated.add(node.target.id)
        store_count = {}
        star_import = False
        for node in ast.walk(tree):
            for name in bound_names(node):
                store_count[name] = store_count.get(name, 0) + 1
            if isinstance(node, ast.ImportFrom) and any(a.name == "*" for a in node.names):
                star_import = True
        defs = [s.name for s in tree.body if isinstance(s, (ast.FunctionDef, ast.AsyncFunctionDef))]
        self.unstable = {}
        if star_import:
            # `from m import *` can rebind any module global when the module runs.
            for name in set(self.functions) | set(assigned):
                self.unstable[name] = "may be rebound by a star import in the module"
        for name in self.functions:
            if name in self.unstable:
                continue
            # A second def, or any assignment to the name, could replace the def we read.
            if defs.count(name) != 1 or store_count.get(name, 0) or name in mutated:
                self.unstable[name] = "is redefined or reassigned in the module"
        for name, values in assigned.items():
            if name in self.functions or name in self.unstable:
                continue
            # Exactly one assignment anywhere in the module, at top level, never mutated.
            if len(values) != 1 or store_count.get(name, 0) != 1 or name in mutated:
                self.unstable[name] = "is assigned more than once or may be mutated"
                continue
            v = values[0]
            try:
                if isinstance(v, (ast.Tuple, ast.List)):
                    self.tables[name] = [const_int(e) for e in v.elts]
                else:
                    self.consts[name] = const_int(v)
            except Reject as r:
                self.unstable[name] = f"is not an int constant or table of ints ({r.msg})"


class FunctionInfo:
    def __init__(self, mod, fn):
        self.mod, self.fn = mod, fn
        if not isinstance(fn, ast.FunctionDef):
            raise Reject(fn, f"async function {fn.name} is not supported")
        if fn.decorator_list:
            raise Reject(fn, f"decorated function {fn.name} is not supported")
        if fn.name in C_RESERVED or fn.name.startswith(RESERVED_PREFIXES):
            raise Reject(fn, f"function name {fn.name} is reserved in the C translation")
        a = fn.args
        if a.vararg or a.kwarg or a.kwonlyargs or a.posonlyargs:
            raise Reject(fn, f"{fn.name}: only plain positional parameters are supported")
        self.params = [(p.arg, annotation_type(p.annotation, "parameter")) for p in a.args]
        self.defaults = [const_int(d) for d in a.defaults]
        n_plain = len(self.params) - len(self.defaults)
        for (name, ty), _ in zip(self.params[n_plain:], self.defaults):
            if ty != INT:
                raise Reject(fn, f"{fn.name}: default for non-int parameter {name}")
        self.locals = set()
        for node in ast.walk(fn):
            if node is not fn and isinstance(
                node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda, ast.ClassDef)
            ):
                raise Reject(node, "nested functions, lambdas and classes are not supported")
            if isinstance(node, (ast.Yield, ast.YieldFrom, ast.Await)):
                raise Reject(node, "generators and coroutines are not supported")
            if isinstance(node, (ast.ListComp, ast.SetComp, ast.DictComp, ast.GeneratorExp)):
                raise Reject(node, "comprehensions are not supported")
            if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store):
                self.locals.add(node.id)
        self.ret_ann = None if fn.returns is None else annotation_type(fn.returns, "return")
        if self.ret_ann in SEQ:
            raise Reject(fn.returns, "only int and bool return types are supported")


class Translator:
    def __init__(self, mod, entry):
        self.mod = mod
        self.infos = {}
        self.order = []
        self.tables_used = set()
        self.consts_used = set()
        self.collect(entry, [])
        self.ret_types = {}
        for name in self.order:
            self.ret_types[name] = self.infos[name].ret_ann
        # Unannotated returns: start at bool and widen to int until stable.
        for name in self.order:
            if self.ret_types[name] is None:
                self.ret_types[name] = BOOL
        changed = True
        while changed:
            changed = False
            for name in self.order:
                info = self.infos[name]
                if info.ret_ann is not None:
                    continue
                env = self.local_types(info)
                rets = [
                    self.expr_type(r.value, env, info)
                    for r in ast.walk(info.fn)
                    if isinstance(r, ast.Return) and r.value is not None
                ]
                t = BOOL if rets and all(r == BOOL for r in rets) else INT
                if t != self.ret_types[name]:
                    self.ret_types[name] = t
                    changed = True
        self.tmp = 0

    # ---- discovery -------------------------------------------------------------------

    def collect(self, name, stack):
        if name in stack:
            raise Reject(self.mod.functions[name], f"recursion through {name} is not supported")
        if name in self.infos:
            return
        fn = self.mod.functions.get(name)
        if fn is None:
            raise Reject(None, f"function {name} is not defined at module top level")
        if name in self.mod.unstable:
            raise Reject(fn, f"function {name} {self.mod.unstable[name]}")
        info = FunctionInfo(self.mod, fn)
        self.infos[name] = info
        for node in ast.walk(fn):
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
                callee = node.func.id
                if callee in self.mod.functions and callee not in info.locals:
                    self.collect(callee, stack + [name])
        self.order.append(name)

    # ---- typing ----------------------------------------------------------------------

    def global_kind(self, name, node, info):
        """What a non-local name refers to."""
        if name in self.mod.unstable:
            raise Reject(node, f"module global {name} {self.mod.unstable[name]}")
        if name in self.mod.consts:
            self.consts_used.add(name)
            return INT
        if name in self.mod.tables:
            self.tables_used.add(name)
            return INTS
        if name in self.mod.functions:
            raise Reject(node, f"function {name} used as a value")
        raise Reject(node, f"unknown name {name}")

    def local_types(self, info):
        """Local variable types: a local is bool only if every value it receives is bool."""
        env = dict(info.params)
        for name in info.locals - set(env):
            env[name] = BOOL
        changed = True
        while changed:
            changed = False
            for name, t in self.assigned_types(info, env):
                if name in dict(info.params):
                    want = dict(info.params)[name]
                    if want in SEQ or (want == BOOL and t != BOOL):
                        raise Reject(info.fn, f"parameter {name} is reassigned with a different type")
                    continue
                if t == INT and env[name] == BOOL:
                    env[name] = INT
                    changed = True
        return env

    def assigned_types(self, info, env):
        out = []
        for node in ast.walk(info.fn):
            if isinstance(node, ast.Assign):
                for t in node.targets:
                    out.extend(self.target_types(t, node.value, env, info))
            elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
                if node.value is not None:
                    out.append((node.target.id, self.expr_type(node.value, env, info)))
            elif isinstance(node, ast.AugAssign) and isinstance(node.target, ast.Name):
                out.append((node.target.id, self.binop_type(node.op, env.get(node.target.id, INT),
                                                            self.expr_type(node.value, env, info))))
            elif isinstance(node, ast.For) and isinstance(node.target, ast.Name):
                out.append((node.target.id, INT))
        return out

    def target_types(self, target, value, env, info):
        if isinstance(target, ast.Name):
            return [(target.id, self.expr_type(value, env, info))]
        if isinstance(target, ast.Tuple) and all(isinstance(e, ast.Name) for e in target.elts):
            if self.is_divmod(value):
                return [(e.id, INT) for e in target.elts]
            if isinstance(value, ast.Tuple) and len(value.elts) == len(target.elts):
                return [(e.id, self.expr_type(v, env, info)) for e, v in zip(target.elts, value.elts)]
        return []

    def is_divmod(self, node):
        return (
            isinstance(node, ast.Call)
            and is_name(node.func, "divmod")
            and len(node.args) == 2
            and not node.keywords
        )

    def binop_type(self, op, a, b):
        if a in SEQ or b in SEQ:
            return None
        if isinstance(op, (ast.BitAnd, ast.BitOr, ast.BitXor)) and a == BOOL and b == BOOL:
            return BOOL
        return INT

    def name_type(self, node, env, info):
        if node.id in info.locals or node.id in env and node.id in dict(info.params):
            return env[node.id]
        return self.global_kind(node.id, node, info)

    def expr_type(self, node, env, info):
        if isinstance(node, ast.Constant):
            if type(node.value) is bool:
                return BOOL
            if type(node.value) is int:
                const_int(node)
                return INT
            raise Reject(node, f"unsupported constant {node.value!r}")
        if isinstance(node, ast.Name):
            return self.name_type(node, env, info)
        if isinstance(node, ast.BinOp):
            a, b = self.expr_type(node.left, env, info), self.expr_type(node.right, env, info)
            t = self.binop_type(node.op, a, b)
            if t is None:
                raise Reject(node, "arithmetic on a sequence is not supported")
            return t
        if isinstance(node, ast.UnaryOp):
            t = self.scalar(node.operand, env, info)
            return BOOL if isinstance(node.op, ast.Not) else INT
        if isinstance(node, ast.BoolOp):
            ts = [self.scalar(v, env, info) for v in node.values]
            return BOOL if all(t == BOOL for t in ts) else INT
        if isinstance(node, ast.Compare):
            for v in [node.left] + node.comparators:
                self.scalar(v, env, info)
            return BOOL
        if isinstance(node, ast.IfExp):
            self.scalar(node.test, env, info)
            a, b = self.scalar(node.body, env, info), self.scalar(node.orelse, env, info)
            return BOOL if a == b == BOOL else INT
        if isinstance(node, ast.Subscript):
            self.seq(node.value, env, info)
            if isinstance(node.slice, ast.Slice):
                raise Reject(node, "slices are not supported")
            self.scalar(node.slice, env, info)
            return INT
        if isinstance(node, ast.Call):
            return self.call_type(node, env, info)
        raise Reject(node, f"unsupported expression {type(node).__name__}")

    def scalar(self, node, env, info):
        t = self.expr_type(node, env, info)
        if t in SEQ:
            raise Reject(node, "a sequence is used where an int or bool is required")
        return t

    def seq(self, node, env, info):
        if not isinstance(node, ast.Name):
            raise Reject(node, "only sequence parameters and module tables can be indexed")
        t = self.expr_type(node, env, info)
        if t not in SEQ:
            raise Reject(node, f"{node.id} is not a sequence")
        return t

    def call_type(self, node, env, info):
        if node.keywords:
            raise Reject(node, "keyword arguments are not supported")
        f = node.func
        if isinstance(f, ast.Attribute):
            if f.attr == "bit_length" and not node.args:
                self.scalar(f.value, env, info)
                return INT
            raise Reject(node, f"unsupported method .{f.attr}()")
        if not isinstance(f, ast.Name):
            raise Reject(node, "unsupported call target")
        name = f.id
        if name in self.mod.functions and name not in info.locals:
            callee = self.infos[name]
            n = len(node.args)
            if not len(callee.params) - len(callee.defaults) <= n <= len(callee.params):
                raise Reject(node, f"{name} takes {len(callee.params)} arguments")
            for arg, (_, pt) in zip(node.args, callee.params):
                at = self.expr_type(arg, env, info)
                if pt in SEQ:
                    if at != pt:
                        raise Reject(arg, f"argument of {name} must be {pt}")
                elif at in SEQ or (pt == BOOL and at != BOOL):
                    raise Reject(arg, f"argument of {name} must be {pt}")
            return self.ret_types[name]
        if name in info.locals or name in self.mod.consts or name in self.mod.tables:
            raise Reject(node, f"{name} is not callable")
        if name == "len" and len(node.args) == 1:
            self.seq(node.args[0], env, info)
            return INT
        if name == "abs" and len(node.args) == 1:
            self.scalar(node.args[0], env, info)
            return INT
        if name in ("min", "max") and len(node.args) >= 2:
            ts = [self.scalar(a, env, info) for a in node.args]
            return BOOL if all(t == BOOL for t in ts) else INT
        if name == "int" and len(node.args) == 1:
            self.scalar(node.args[0], env, info)
            return INT
        if name == "bool" and len(node.args) == 1:
            self.scalar(node.args[0], env, info)
            return BOOL
        raise Reject(node, f"unsupported call {name}()")

    # ---- definite assignment ---------------------------------------------------------

    def check_defined(self, stmts, defined, info):
        """Returns the names defined after `stmts`, or None when control cannot fall through."""
        for s in stmts:
            defined = self.check_stmt(s, defined, info)
            if defined is None:
                return None
        return defined

    def uses(self, node, defined, info):
        for n in ast.walk(node):
            if (
                isinstance(n, ast.Name)
                and isinstance(n.ctx, ast.Load)
                and n.id in info.locals
                and n.id not in defined
            ):
                raise Reject(n, f"local {n.id} may be read before it is assigned")

    def check_stmt(self, s, defined, info):
        if isinstance(s, (ast.Assign, ast.AnnAssign, ast.AugAssign)):
            if s.value is not None:
                self.uses(s.value, defined, info)
            targets = s.targets if isinstance(s, ast.Assign) else [s.target]
            if isinstance(s, ast.AugAssign):
                self.uses(ast.Name(id=s.target.id, ctx=ast.Load()), defined, info)
            if s.value is None:
                return defined
            new = set(defined)
            for t in targets:
                for n in ast.walk(t):
                    if isinstance(n, ast.Name):
                        new.add(n.id)
            return new
        if isinstance(s, ast.If):
            self.uses(s.test, defined, info)
            a = self.check_defined(s.body, set(defined), info)
            b = self.check_defined(s.orelse, set(defined), info)
            if a is None:
                return b
            if b is None:
                return a
            return a & b
        if isinstance(s, ast.While):
            self.uses(s.test, defined, info)
            self.check_defined(s.body, set(defined), info)
            return defined
        if isinstance(s, ast.For):
            self.uses(s.iter, defined, info)
            self.check_defined(s.body, set(defined) | {s.target.id}, info)
            return defined
        if isinstance(s, ast.Return):
            if s.value is not None:
                self.uses(s.value, defined, info)
            return None
        if isinstance(s, (ast.Raise, ast.Break, ast.Continue)):
            return None
        if isinstance(s, ast.Assert):
            self.uses(s.test, defined, info)
            return defined
        return defined

    # ---- emission --------------------------------------------------------------------

    def fresh(self):
        self.tmp += 1
        return f"py_t{self.tmp}"


    def var(self, name, info):
        return f"v_{name}"

    def ex(self, node, env, info):
        """C expression for a scalar Python expression (int64_t or bool valued)."""
        if isinstance(node, ast.Constant):
            if type(node.value) is bool:
                return "INT64_C(1)" if node.value else "INT64_C(0)"
            return c_int(const_int(node))
        if isinstance(node, ast.Name):
            if node.id in info.locals or node.id in dict(info.params):
                return self.var(node.id, info)
            self.global_kind(node.id, node, info)
            return f"g_{node.id}"
        if isinstance(node, ast.UnaryOp):
            # Fold -<literal> so INT64_MIN can be written.
            if isinstance(node.op, ast.USub) and isinstance(node.operand, ast.Constant):
                return c_int(const_int(node))
            v = self.i64(node.operand, env, info)
            if isinstance(node.op, ast.USub):
                return f"py_neg({v}, py_e)"
            if isinstance(node.op, ast.UAdd):
                return v
            if isinstance(node.op, ast.Invert):
                return f"(~{v})"
            return f"(!{self.truth(node.operand, env, info)})"
        if isinstance(node, ast.BinOp):
            t = self.expr_type(node, env, info)
            a, b = self.i64(node.left, env, info), self.i64(node.right, env, info)
            op = type(node.op)
            bitwise = {ast.BitAnd: "&", ast.BitOr: "|", ast.BitXor: "^"}
            if op in bitwise:
                return f"({a} {bitwise[op]} {b})"  # bool & bool stays 0 or 1
            helper = {ast.Add: "py_add", ast.Sub: "py_sub", ast.Mult: "py_mul",
                      ast.FloorDiv: "py_floordiv", ast.Mod: "py_mod", ast.LShift: "py_lshift",
                      ast.RShift: "py_rshift", ast.Pow: "py_pow"}.get(op)
            if helper is None:
                raise Reject(node, f"unsupported operator {type(node.op).__name__}")
            return f"{helper}({a}, {b}, py_e)"
        if isinstance(node, ast.BoolOp):
            t = self.expr_type(node, env, info)
            if t == BOOL:
                eager = all(self.error_free(v) for v in node.values)
                sep = {(True, True): " & ", (True, False): " | ",
                       (False, True): " && ", (False, False): " || "}[
                    (eager, isinstance(node.op, ast.And))]
                return "(" + sep.join(self.ex(v, env, info) for v in node.values) + ")"
            # Python returns an operand: `a and b` is b when a is truthy, else a.
            acc = self.i64(node.values[-1], env, info)
            for v in reversed(node.values[:-1]):
                x, tx = self.i64(v, env, info), self.truth(v, env, info)
                acc = f"({tx} ? {acc} : {x})" if isinstance(node.op, ast.And) else f"({tx} ? {x} : {acc})"
            return acc
        if isinstance(node, ast.Compare):
            ops = {ast.Lt: "<", ast.LtE: "<=", ast.Gt: ">", ast.GtE: ">=", ast.Eq: "==",
                   ast.NotEq: "!="}
            parts = []
            left = node.left
            for op, right in zip(node.ops, node.comparators):
                if type(op) not in ops:
                    raise Reject(node, f"unsupported comparison {type(op).__name__}")
                parts.append(f"({self.i64(left, env, info)} {ops[type(op)]} {self.i64(right, env, info)})")
                left = right
            # A later comparison runs only when the earlier ones held; eager when nothing can fail.
            sep = " & " if self.error_free(node) else " && "
            return "(" + sep.join(parts) + ")"
        if isinstance(node, ast.IfExp):
            t = self.expr_type(node, env, info)
            body, orelse = (
                (self.ex(node.body, env, info), self.ex(node.orelse, env, info))
                if t == BOOL
                else (self.i64(node.body, env, info), self.i64(node.orelse, env, info))
            )
            if self.error_free(node):
                return f"py_sel({self.truth(node.test, env, info)}, {body}, {orelse})"
            return f"({self.truth(node.test, env, info)} ? {body} : {orelse})"
        if isinstance(node, ast.Subscript):
            t = self.expr_type(node.value, env, info)
            base = self.ex(node.value, env, info)
            idx = self.i64(node.slice, env, info)
            if node.value.id in self.mod.tables and node.value.id not in info.locals:
                return f"py_load_table({base}, {base}_len, {idx}, py_e)"
            load = "py_load_bytes" if t == BYTES else "py_load_ints"
            return f"{load}({base}, {base}_len, {idx}, py_e)"
        if isinstance(node, ast.Call):
            return self.call(node, env, info)
        raise Reject(node, f"unsupported expression {type(node).__name__}")

    def error_free(self, node):
        """True when evaluating node can never set the error flag, so evaluating it eagerly
        instead of short-circuiting changes nothing: names, constants and their comparisons
        and bitwise or boolean combinations."""
        if isinstance(node, (ast.Name, ast.Constant)):
            return True
        if isinstance(node, ast.UnaryOp):
            return isinstance(node.op, (ast.Not, ast.Invert, ast.UAdd)) and self.error_free(node.operand)
        if isinstance(node, ast.BinOp):
            return isinstance(node.op, (ast.BitAnd, ast.BitOr, ast.BitXor)) and all(
                self.error_free(x) for x in (node.left, node.right))
        if isinstance(node, ast.Compare):
            return all(self.error_free(x) for x in [node.left, *node.comparators])
        if isinstance(node, ast.BoolOp):
            return all(self.error_free(x) for x in node.values)
        if isinstance(node, ast.IfExp):
            return all(self.error_free(x) for x in (node.test, node.body, node.orelse))
        return False

    def i64(self, node, env, info):
        return f"((int64_t){self.ex(node, env, info)})"

    def truth(self, node, env, info):
        t = self.scalar(node, env, info)
        return self.ex(node, env, info) if t == BOOL else f"({self.ex(node, env, info)} != 0)"

    def call(self, node, env, info):
        t = self.call_type(node, env, info)
        f = node.func
        if isinstance(f, ast.Attribute):
            return f"py_bit_length({self.i64(f.value, env, info)})"
        name = f.id
        if name in self.mod.functions and name not in info.locals:
            callee = self.infos[name]
            args = []
            n_plain = len(callee.params) - len(callee.defaults)
            for i, (_, pt) in enumerate(callee.params):
                if i < len(node.args):
                    arg = node.args[i]
                    if pt in SEQ:
                        base = self.ex(arg, env, info)
                        args += [base, f"{base}_len"]
                    else:
                        args.append(self.i64(arg, env, info))
                else:
                    args.append(c_int(callee.defaults[i - n_plain]))
            return f"py_impl_{name}({', '.join(args + ['py_e'])})"
        a = [self.i64(x, env, info) for x in node.args] if name not in ("len", "bool") else []
        if name == "len":
            return f"((int64_t){self.ex(node.args[0], env, info)}_len)"
        if name == "abs":
            return f"py_abs({a[0]}, py_e)"
        if name in ("min", "max"):
            acc = a[0]
            for x in a[1:]:
                acc = f"py_{name}({acc}, {x})"
            return acc
        if name == "int":
            return a[0]
        if name == "bool":
            return self.truth(node.args[0], env, info)
        raise Reject(node, f"unsupported call {name}()")

    def assign(self, name, value_c, env, info, out, ind):
        out.append(f"{ind}{self.var(name, info)} = {value_c};")

    def scalar_value(self, node, env, info, dest_type):
        return self.i64(node, env, info)

    def stmts(self, body, env, info, out, ind, ret_t):
        for s in body:
            self.stmt(s, env, info, out, ind, ret_t)

    def stmt(self, s, env, info, out, ind, ret_t):
        if isinstance(s, ast.Expr):
            if isinstance(s.value, ast.Constant) and isinstance(s.value.value, str):
                return  # docstring
            raise Reject(s, "expression statements are not supported (no effects to keep)")
        if isinstance(s, ast.Pass):
            return
        if isinstance(s, ast.Assign):
            if len(s.targets) != 1:
                raise Reject(s, "chained assignment is not supported")
            target = s.targets[0]
            if isinstance(target, ast.Name):
                self.check_assignable(target.id, s, info)
                self.scalar(s.value, env, info)
                self.assign(target.id, self.scalar_value(s.value, env, info, env[target.id]), env, info, out, ind)
                return
            if isinstance(target, ast.Tuple) and all(isinstance(e, ast.Name) for e in target.elts):
                names = [e.id for e in target.elts]
                for n in names:
                    self.check_assignable(n, s, info)
                if self.is_divmod(s.value):
                    a, b = self.fresh(), self.fresh()
                    out.append(f"{ind}{{ int64_t {a} = {self.i64(s.value.args[0], env, info)}; "
                               f"int64_t {b} = {self.i64(s.value.args[1], env, info)};")
                    if len(names) != 2:
                        raise Reject(s, "divmod unpacks into exactly two names")
                    self.assign(names[0], f"py_floordiv({a}, {b}, py_e)", env, info, out, ind + "  ")
                    self.assign(names[1], f"py_mod({a}, {b}, py_e)", env, info, out, ind + "  ")
                    out.append(f"{ind}}}")
                    return
                if isinstance(s.value, ast.Tuple) and len(s.value.elts) == len(names):
                    # Python evaluates the whole right side before assigning.
                    temps = []
                    out.append(f"{ind}{{")
                    for n, v in zip(names, s.value.elts):
                        self.scalar(v, env, info)
                        tmp = self.fresh()
                        temps.append(tmp)
                        out.append(f"{ind}  int64_t {tmp} = {self.scalar_value(v, env, info, env[n])};")
                    for n, tmp in zip(names, temps):
                        self.assign(n, tmp, env, info, out, ind + "  ")
                    out.append(f"{ind}}}")
                    return
            raise Reject(s, "only names and tuples of names can be assigned")
        if isinstance(s, ast.AnnAssign):
            if not isinstance(s.target, ast.Name):
                raise Reject(s, "only names can be assigned")
            t = annotation_type(s.annotation, "variable")
            if s.value is None:
                return
            self.check_assignable(s.target.id, s, info)
            vt = self.scalar(s.value, env, info)
            if t == BOOL and vt != BOOL or t in SEQ:
                raise Reject(s, f"value does not match annotation {ast.unparse(s.annotation)}")
            self.assign(s.target.id, self.scalar_value(s.value, env, info, env[s.target.id]), env, info, out, ind)
            return
        if isinstance(s, ast.AugAssign):
            if not isinstance(s.target, ast.Name):
                raise Reject(s, "only names can be augmented")
            self.check_assignable(s.target.id, s, info)
            fake = ast.BinOp(left=ast.Name(id=s.target.id, ctx=ast.Load()), op=s.op, right=s.value)
            ast.copy_location(fake, s)
            ast.copy_location(fake.left, s)
            self.scalar(fake, env, info)
            self.assign(s.target.id, self.scalar_value(fake, env, info, env[s.target.id]), env, info, out, ind)
            return
        if isinstance(s, ast.If):
            out.append(f"{ind}if ({self.truth(s.test, env, info)}) {{")
            self.stmts(s.body, env, info, out, ind + "  ", ret_t)
            out.append(f"{ind}}} else {{")
            self.stmts(s.orelse, env, info, out, ind + "  ", ret_t)
            out.append(f"{ind}}}")
            return
        if isinstance(s, ast.While):
            if s.orelse:
                raise Reject(s, "while-else is not supported")
            out.append(f"{ind}while ({self.truth(s.test, env, info)}) {{")
            self.stmts(s.body, env, info, out, ind + "  ", ret_t)
            out.append(f"{ind}}}")
            return
        if isinstance(s, ast.For):
            self.for_stmt(s, env, info, out, ind, ret_t)
            return
        if isinstance(s, ast.Break):
            out.append(f"{ind}break;")
            return
        if isinstance(s, ast.Continue):
            out.append(f"{ind}continue;")
            return
        if isinstance(s, ast.Return):
            if s.value is None:
                raise Reject(s, "a bare return gives None, which is not an int or bool")
            vt = self.scalar(s.value, env, info)
            if ret_t == BOOL and vt != BOOL:
                raise Reject(s, "returns an int from a function whose return type is bool")
            out.append(f"{ind}return {self.i64(s.value, env, info)};")
            return
        if isinstance(s, ast.Raise):
            # Any exception is Err; the exception object itself is never observed.
            out.append(f"{ind}*py_e = 1; return 0;")
            return
        if isinstance(s, ast.Assert):
            out.append(f"{ind}*py_e |= !{self.truth(s.test, env, info)};")
            return
        raise Reject(s, f"unsupported statement {type(s).__name__}")

    def check_assignable(self, name, node, info):
        params = dict(info.params)
        if name in params and params[name] in SEQ:
            raise Reject(node, f"sequence parameter {name} cannot be reassigned")

    def for_stmt(self, s, env, info, out, ind, ret_t):
        if s.orelse:
            raise Reject(s, "for-else is not supported")
        if not isinstance(s.target, ast.Name):
            raise Reject(s, "the loop target must be a single name")
        self.check_assignable(s.target.id, s, info)
        it = s.iter
        k = self.fresh()
        if isinstance(it, ast.Call) and is_name(it.func, "range") and not it.keywords:
            if not 1 <= len(it.args) <= 3:
                raise Reject(it, "range takes one to three arguments")
            for a in it.args:
                self.scalar(a, env, info)
            start = self.i64(it.args[0], env, info) if len(it.args) > 1 else "INT64_C(0)"
            stop = self.i64(it.args[-1] if len(it.args) == 1 else it.args[1], env, info)
            step = 1
            if len(it.args) == 3:
                try:
                    step = const_int(it.args[2])
                except Reject:
                    raise Reject(it.args[2], "the range step must be a constant") from None
                if step == 0:
                    raise Reject(it.args[2], "range step is zero")
            # range evaluates its arguments once and the body cannot change the iteration:
            # a counted loop over its length, whose counter cannot overflow.
            start_v, n_v = self.fresh(), self.fresh()
            out.append(f"{ind}{{ int64_t {start_v} = {start}; "
                       f"u64 {n_v} = py_range_len({start_v}, {stop}, {c_int(step)});")
            out.append(f"{ind}for (u64 {k} = 0; {k} < {n_v}; {k}++) {{")
            self.assign(s.target.id, f"(int64_t)((u64){start_v} + {k} * (u64){c_int(step)})",
                        env, info, out, ind + "  ")
            self.stmts(s.body, env, info, out, ind + "  ", ret_t)
            out.append(f"{ind}}} }}")
            return
        if isinstance(it, ast.Name):
            self.seq(it, env, info)
            base = self.ex(it, env, info)
            out.append(f"{ind}for (size_t {k} = 0; {k} < {base}_len; {k}++) {{")
            self.assign(s.target.id, f"(int64_t){base}[{k}]", env, info, out, ind + "  ")
            self.stmts(s.body, env, info, out, ind + "  ", ret_t)
            out.append(f"{ind}}}")
            return
        raise Reject(it, "only range(...) and sequences can be iterated")

    def function(self, name):
        info = self.infos[name]
        env = self.local_types(info)
        self.check_defined(info.fn.body, {p for p, _ in info.params}, info)
        ret_t = self.ret_types[name]
        params, args = [], []
        for p, t in info.params:
            if t in SEQ:
                elem = "uint8_t" if t == BYTES else "int64_t"
                params += [f"const {elem} *v_{p}", f"size_t v_{p}_len"]
                args += [f"v_{p}", f"v_{p}_len"]
            else:
                params.append(f"int64_t v_{p}")
                args.append(f"v_{p}")
        out = [f"PY_INLINE int64_t py_impl_{name}({', '.join(params + ['int64_t *py_e'])}) {{"]
        for p, t in info.params:
            if t == BOOL:  # True and False only
                out.append(f"  *py_e |= (u64)v_{p} > 1;")
        for local in sorted(info.locals - {p for p, _ in info.params}):
            out.append(f"  int64_t v_{local} = 0;")
        self.stmts(info.fn.body, env, info, out, "  ", ret_t)
        # Falling off the end returns None in Python, which is not an int or bool.
        out.append("  *py_e = 1; return 0;")
        out.append("}")
        if name == self.order[-1]:
            out += [
                f"int64_t {name}({', '.join(params) or 'void'}) {{",
                "  int64_t py_e = 0;",
                f"  int64_t py_v = py_impl_{name}({', '.join(args + ['&py_e'])});",
                "  return py_fail(py_e, py_v);",
                "}",
            ]
        return "\n".join(out)

    def translate(self):
        # Callees come before callers in self.order, so no prototypes are needed.
        bodies = [self.function(name) for name in self.order]
        globals_c = []
        for name in sorted(self.consts_used):
            globals_c.append(f"static const int64_t g_{name} = {c_int(self.mod.consts[name])};")
        for name in sorted(self.tables_used):
            vals = self.mod.tables[name]
            if not vals:
                raise Reject(None, f"module table {name} is empty")
            globals_c.append(
                f"static const int64_t g_{name}[{len(vals)}] = {{{', '.join(c_int(v) for v in vals)}}};"
            )
            globals_c.append(f"static const size_t g_{name}_len = {len(vals)};")
        return "\n".join([PRELUDE, *globals_c, "", *bodies, ""])


def main(argv):
    if len(argv) != 4:
        print("usage: pylift.py SOURCE.py FUNCTION OUT.c", file=sys.stderr)
        return 64
    src_path, entry, out_path = argv[1:]
    with open(src_path, encoding="utf-8") as f:
        src = f.read()
    try:
        tree = ast.parse(src, filename=src_path)
        mod = ModuleInfo(tree)
        if entry not in mod.functions:
            raise Reject(None, f"function {entry} is not defined at module top level")
        c = Translator(mod, entry).translate()
    except SyntaxError as e:
        print(f"pylift: {e.lineno}:{e.offset}: syntax error: {e.msg}", file=sys.stderr)
        return 2
    except Reject as r:
        line = getattr(r.node, "lineno", 0)
        col = getattr(r.node, "col_offset", 0)
        print(f"pylift: {line}:{col}: {r.msg}", file=sys.stderr)
        return 2
    with open(out_path, "w", encoding="utf-8") as f:
        f.write(c)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
