//! Concrete evaluator for canonical VG-IR nodes (docs/13, docs/14).

use vg_core::ir::{Hash, Node, Op, Operand};
use vg_core::opcode;
use vg_core::types::Type;
use vg_core::value::{Value, int_fits};

use crate::bytes::{decode_const, validate_and_canonicalize, value_to_bytes};
use crate::float_ops;
use crate::formula::{Clause, holds_with_depth};
use crate::host::{Host, NoEffectsHost, Trap};

/// Evaluates `node` concretely on `inputs` (node parameters followed by learnable θ in declaration
/// order), resolving callees via `callee` and executing effect ops against `host`.
pub fn run(
    node: &Node,
    inputs: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
    host: &mut dyn Host,
) -> Result<Vec<Value>, Trap> {
    let mut last_clock = None;
    run_inner_with_depth(node, inputs, callee, host, &mut last_clock, 0)
}

pub(crate) fn run_inner_with_depth(
    node: &Node,
    inputs: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
    host: &mut dyn Host,
    last_clock: &mut Option<u64>,
    depth: usize,
) -> Result<Vec<Value>, Trap> {
    if depth > 256 {
        return Err(Trap::entry("maximum call depth exceeded"));
    }
    if inputs.len() != node.input_count() {
        return Err(Trap::entry(format!(
            "expected {} inputs (params + learnable theta), got {}",
            node.input_count(),
            inputs.len()
        )));
    }
    let mut norm_inputs = Vec::with_capacity(inputs.len());
    for (i, val) in inputs.iter().enumerate() {
        let ty = node
            .input_type(i as u32)
            .ok_or_else(|| Trap::entry(format!("input index {i} out of range")))?;
        let canon = validate_and_canonicalize(ty, val)
            .map_err(|e| Trap::entry(format!("input #{i} ({ty}): {e}")))?;
        norm_inputs.push(canon);
    }

    let mut const_vals = Vec::with_capacity(node.consts.len());
    for (ci, c) in node.consts.iter().enumerate() {
        if c.ty == Type::Int {
            // Formula-only Int constant; keep a placeholder so op evaluation traps if referenced.
            const_vals.push(None);
        } else {
            let v = decode_const(c)
                .map_err(|e| Trap::entry(format!("constant #{ci} ({}): {e}", c.ty)))?;
            const_vals.push(Some(v));
        }
    }

    let mut op_results: Vec<Vec<Value>> = Vec::with_capacity(node.ops.len());
    for (idx, op) in node.ops.iter().enumerate() {
        let res = eval_op(
            node,
            idx as u32,
            op,
            &norm_inputs,
            &const_vals,
            &op_results,
            callee,
            host,
            last_clock,
            depth,
        )?;
        if res.len() != op.results.len() {
            return Err(Trap::op(
                idx as u32,
                opcode_name(op.opcode),
                format!(
                    "expected {} results, produced {}",
                    op.results.len(),
                    res.len()
                ),
            ));
        }
        op_results.push(res);
    }

    if node.outputs.len() != node.results.len() {
        return Err(Trap::entry(format!(
            "node has {} outputs for {} signature results",
            node.outputs.len(),
            node.results.len()
        )));
    }
    let mut outputs = Vec::with_capacity(node.outputs.len());
    for (j, o) in node.outputs.iter().enumerate() {
        let v = read_val(*o, &norm_inputs, &const_vals, &op_results)
            .map_err(|m| Trap::entry(format!("output #{j}: {m}")))?;
        let ty = &node.results[j];
        let v = validate_and_canonicalize(ty, &v)
            .map_err(|e| Trap::entry(format!("output #{j} ({ty}): {e}")))?;
        outputs.push(v);
    }
    Ok(outputs)
}

fn opcode_name(code: u16) -> &'static str {
    opcode::by_code(code).map_or("UNKNOWN", |i| i.name)
}

fn read_val(
    o: Operand,
    inputs: &[Value],
    consts: &[Option<Value>],
    op_results: &[Vec<Value>],
) -> Result<Value, String> {
    match o {
        Operand::Input(i) => inputs
            .get(i as usize)
            .cloned()
            .ok_or_else(|| format!("input index {i} out of range")),
        Operand::Op(i, slot) => op_results
            .get(i as usize)
            .and_then(|rs| rs.get(slot as usize))
            .cloned()
            .ok_or_else(|| format!("op result ({i}, {slot}) out of range")),
        Operand::Const(c) => consts
            .get(c as usize)
            .and_then(|v| v.clone())
            .ok_or_else(|| format!("const index {c} missing or non-value Int constant")),
        Operand::Callee(_) | Operand::Invariant(_) => {
            Err(format!("operand {o:?} does not hold a runtime value"))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn eval_op(
    node: &Node,
    op_idx: u32,
    op: &Op,
    inputs: &[Value],
    consts: &[Option<Value>],
    op_results: &[Vec<Value>],
    callee: &dyn Fn(&Hash) -> Option<Node>,
    host: &mut dyn Host,
    last_clock: &mut Option<u64>,
    depth: usize,
) -> Result<Vec<Value>, Trap> {
    let name = opcode_name(op.opcode);
    let trap = |msg: String| Trap::op(op_idx, name, msg);
    let val = |slot: usize| -> Result<Value, Trap> {
        let operand = *op
            .operands
            .get(slot)
            .ok_or_else(|| trap(format!("{name}: missing operand #{slot}")))?;
        read_val(operand, inputs, consts, op_results).map_err(trap)
    };
    let val_ty = |slot: usize| -> Result<&Type, Trap> {
        let operand = *op
            .operands
            .get(slot)
            .ok_or_else(|| trap(format!("{name}: missing operand #{slot}")))?;
        node.operand_type(operand)
            .ok_or_else(|| trap(format!("{name}: operand #{slot} has no value type")))
    };
    let callee_hash = |slot: usize| -> Result<Hash, Trap> {
        match op.operands.get(slot) {
            Some(Operand::Callee(c)) => node
                .calls
                .get(*c as usize)
                .copied()
                .ok_or_else(|| trap(format!("{name}: callee index {c} out of range"))),
            other => Err(trap(format!(
                "{name}: operand #{slot} must be a callee, got {other:?}"
            ))),
        }
    };
    let as_bool = |v: &Value, arg: &str| -> Result<bool, Trap> {
        match v {
            Value::Bool(b) => Ok(*b),
            _ => Err(trap(format!("{name}: expected Bool for {arg}, got {v:?}"))),
        }
    };
    let as_int = |v: &Value, arg: &str| -> Result<i128, Trap> {
        match v {
            Value::Int(x) => Ok(*x),
            _ => Err(trap(format!("{name}: expected Int for {arg}, got {v:?}"))),
        }
    };
    let as_float = |v: &Value, arg: &str| -> Result<u64, Trap> {
        match v {
            Value::Float(bits) => Ok(*bits),
            _ => Err(trap(format!("{name}: expected Float for {arg}, got {v:?}"))),
        }
    };

    match op.opcode {
        // 0x0001..0x000C Structure
        0x0003 => {
            // TUPLE
            let mut members = Vec::with_capacity(op.operands.len());
            for i in 0..op.operands.len() {
                members.push(val(i)?);
            }
            Ok(vec![Value::Tuple(members)])
        }
        0x0004 => {
            // PROJ(t, j)
            let t = val(0)?;
            let j = as_int(&val(1)?, "index")?;
            let Value::Tuple(elems) = t else {
                return Err(trap(format!("PROJ: expected Tuple, got {t:?}")));
            };
            let idx = usize::try_from(j)
                .ok()
                .filter(|&k| k < elems.len())
                .ok_or_else(|| {
                    trap(format!(
                        "PROJ: index {j} out of range for tuple len {}",
                        elems.len()
                    ))
                })?;
            Ok(vec![elems[idx].clone()])
        }
        0x0005 => {
            // SELECT(c, a, b)
            let c = as_bool(&val(0)?, "condition")?;
            let a = val(1)?;
            let b = val(2)?;
            Ok(vec![if c { a } else { b }])
        }
        0x0006 => {
            // CAST.<S2>(v)
            let src_ty = val_ty(0)?;
            let dst_ty = op
                .results
                .first()
                .ok_or_else(|| trap("CAST: missing result type".to_string()))?;
            if src_ty == dst_ty {
                return Err(trap(format!(
                    "CAST: trivial cast {src_ty} -> {dst_ty} disallowed"
                )));
            }
            let v = val(0)?;
            let out = match (src_ty, dst_ty) {
                (Type::Bool, t) if t.is_int() => {
                    let b = as_bool(&v, "src")?;
                    Value::Int(i128::from(u8::from(b)))
                }
                (s, t) if s.is_int() && t.is_int() => {
                    let x = as_int(&v, "src")?;
                    if !int_fits(x, t) {
                        return Err(trap(format!(
                            "CAST.in_range({src_ty} -> {dst_ty}): value {x} out of range"
                        )));
                    }
                    Value::Int(x)
                }
                (s, t) if s.is_int() && t.is_float() => {
                    let x = as_int(&v, "src")?;
                    Value::Float(float_ops::int_to_float(x, t))
                }
                (s, t) if s.is_float() && t.is_int() => {
                    let bits = as_float(&v, "src")?;
                    let x = float_ops::float_to_int(s, bits, t).map_err(|obl| {
                        trap(format!(
                            "{obl}({src_ty} -> {dst_ty}): value bits {bits:#x} out of range"
                        ))
                    })?;
                    Value::Int(x)
                }
                (s, t) if s.is_float() && t.is_float() => {
                    let bits = as_float(&v, "src")?;
                    Value::Float(float_ops::float_to_float(s, bits, t))
                }
                _ => {
                    return Err(trap(format!(
                        "CAST: disallowed conversion {src_ty} -> {dst_ty} on {v:?}"
                    )));
                }
            };
            Ok(vec![out])
        }
        0x0007 => {
            // AS_BYTES(v)
            let src_ty = val_ty(0)?;
            let v = val(0)?;
            let bytes = value_to_bytes(src_ty, &v)
                .map_err(|e| trap(format!("AS_BYTES({src_ty}, {v:?}): {e}")))?;
            Ok(vec![Value::Seq(
                bytes
                    .into_iter()
                    .map(|b| Value::Int(i128::from(b)))
                    .collect(),
            )])
        }
        0x0008 => Ok(vec![Value::Ok(Box::new(val(0)?))]), // OK
        0x0009 => Ok(vec![Value::Err(Box::new(val(0)?))]), // ERR
        0x000A => {
            // IS_OK(r)
            match val(0)? {
                Value::Ok(_) => Ok(vec![Value::Bool(true)]),
                Value::Err(_) => Ok(vec![Value::Bool(false)]),
                other => Err(trap(format!("IS_OK: expected Result, got {other:?}"))),
            }
        }
        0x000B => {
            // GET_OK(r)
            match val(0)? {
                Value::Ok(v) => Ok(vec![*v]),
                Value::Err(e) => Err(trap(format!("GET_OK.is_ok: called on Err({e:?})"))),
                other => Err(trap(format!("GET_OK: expected Result, got {other:?}"))),
            }
        }
        0x000C => {
            // GET_ERR(r)
            match val(0)? {
                Value::Err(e) => Ok(vec![*e]),
                Value::Ok(v) => Err(trap(format!("GET_ERR.is_err: called on Ok({v:?})"))),
                other => Err(trap(format!("GET_ERR: expected Result, got {other:?}"))),
            }
        }

        // 0x0010..0x0015 Integer arithmetic
        0x0010 => {
            // ADD.<checked|wrap|sat>(a, b)
            let ty = val_ty(0)?;
            let a = as_int(&val(0)?, "a")?;
            let b = as_int(&val(1)?, "b")?;
            let sum = a + b;
            int_arith_result(op.mode, "ADD", ty, a, b, sum).map_err(trap)
        }
        0x0011 => {
            // SUB.<checked|wrap|sat>(a, b)
            let ty = val_ty(0)?;
            let a = as_int(&val(0)?, "a")?;
            let b = as_int(&val(1)?, "b")?;
            let diff = a - b;
            int_arith_result(op.mode, "SUB", ty, a, b, diff).map_err(trap)
        }
        0x0012 => {
            // MUL.<checked|wrap|sat>(a, b)
            let ty = val_ty(0)?;
            let a = as_int(&val(0)?, "a")?;
            let b = as_int(&val(1)?, "b")?;
            let out = match ty {
                Type::UInt(w) => {
                    let prod = (a as u128) * (b as u128);
                    let mask = if *w == 64 {
                        u64::MAX as u128
                    } else {
                        (1u128 << *w) - 1
                    };
                    match op.mode {
                        0 => {
                            if prod > mask {
                                return Err(trap(format!(
                                    "MUL.no_overflow({ty}): {a} * {b} overflows {ty}"
                                )));
                            }
                            prod as i128
                        }
                        1 => (prod & mask) as i128,
                        2 => prod.min(mask) as i128,
                        m => return Err(trap(format!("MUL: unknown mode {m}"))),
                    }
                }
                Type::SInt(_) => int_arith_result(op.mode, "MUL", ty, a, b, a * b).map_err(trap)?
                    [0]
                .clone()
                .into_int()?,
                _ => return Err(trap(format!("MUL: expected integer type, got {ty}"))),
            };
            Ok(vec![Value::Int(out)])
        }
        0x0013 => {
            // DIV.<checked|wrap|sat>(a, b)
            let ty = val_ty(0)?;
            let a = as_int(&val(0)?, "a")?;
            let b = as_int(&val(1)?, "b")?;
            if b == 0 {
                return Err(trap(format!("DIV.nonzero({ty}): division of {a} by 0")));
            }
            let (lo, hi) = int_bounds(ty).map_err(trap)?;
            let out = if matches!(ty, Type::SInt(_)) && a == lo && b == -1 {
                match op.mode {
                    0 => {
                        return Err(trap(format!(
                            "DIV.no_overflow({ty}): {a} / {b} overflows {ty}"
                        )));
                    }
                    1 => lo,
                    2 => hi,
                    m => return Err(trap(format!("DIV: unknown mode {m}"))),
                }
            } else if op.mode <= 2 {
                a / b
            } else {
                return Err(trap(format!("DIV: unknown mode {}", op.mode)));
            };
            Ok(vec![Value::Int(out)])
        }
        0x0014 => {
            // MOD.<checked|wrap|sat>(a, b)
            let ty = val_ty(0)?;
            let a = as_int(&val(0)?, "a")?;
            let b = as_int(&val(1)?, "b")?;
            if b == 0 {
                return Err(trap(format!("MOD.nonzero({ty}): modulo of {a} by 0")));
            }
            if op.mode > 2 {
                return Err(trap(format!("MOD: unknown mode {}", op.mode)));
            }
            let (lo, _) = int_bounds(ty).map_err(trap)?;
            let out = if matches!(ty, Type::SInt(_)) && a == lo && b == -1 {
                0
            } else {
                a % b
            };
            Ok(vec![Value::Int(out)])
        }
        0x0015 => {
            // NEG.<checked|wrap|sat>(a)
            let ty = val_ty(0)?;
            let a = as_int(&val(0)?, "a")?;
            let neg = -a;
            let (lo, hi) = int_bounds(ty).map_err(trap)?;
            let out = match op.mode {
                0 => {
                    if !int_fits(neg, ty) {
                        return Err(trap(format!(
                            "NEG.no_overflow({ty}): -{a} does not fit {ty}"
                        )));
                    }
                    neg
                }
                1 => wrap_int(neg, ty).map_err(trap)?,
                2 => neg.clamp(lo, hi),
                m => return Err(trap(format!("NEG: unknown mode {m}"))),
            };
            Ok(vec![Value::Int(out)])
        }

        // 0x0030..0x0037 Bits
        0x0030 => bitwise_bin(
            val_ty(0)?,
            as_int(&val(0)?, "a")?,
            as_int(&val(1)?, "b")?,
            |x, y| x & y,
        )
        .map_err(trap),
        0x0031 => bitwise_bin(
            val_ty(0)?,
            as_int(&val(0)?, "a")?,
            as_int(&val(1)?, "b")?,
            |x, y| x | y,
        )
        .map_err(trap),
        0x0032 => bitwise_bin(
            val_ty(0)?,
            as_int(&val(0)?, "a")?,
            as_int(&val(1)?, "b")?,
            |x, y| x ^ y,
        )
        .map_err(trap),
        0x0033 => {
            let ty = val_ty(0)?;
            let a = as_int(&val(0)?, "a")?;
            let w = int_width(ty).map_err(trap)?;
            let u = int_to_bits(a, w);
            Ok(vec![Value::Int(bits_to_int(!u, ty))])
        }
        0x0034 | 0x0035 => {
            // SHL / SHR .<checked|wrap>(a, s)
            let ty = val_ty(0)?;
            let w = int_width(ty).map_err(trap)?;
            let a = as_int(&val(0)?, "a")?;
            let s = as_int(&val(1)?, "shift")?;
            let su =
                u32::try_from(s).map_err(|_| trap(format!("{name}: invalid shift amount {s}")))?;
            let sh = match op.mode {
                0 => {
                    if su >= u32::from(w) {
                        return Err(trap(format!(
                            "{name}.amount({ty}): shift {su} >= bit width {w} on {a}"
                        )));
                    }
                    su
                }
                1 => su % u32::from(w),
                m => return Err(trap(format!("{name}: unknown mode {m}"))),
            };
            let bits = int_to_bits(a, w);
            let res_bits = if op.opcode == 0x0034 {
                bits << sh
            } else if matches!(ty, Type::SInt(_)) {
                let sa = ((bits as i64) << (64 - u32::from(w))) >> (64 - u32::from(w));
                (sa >> sh) as u64
            } else {
                bits >> sh
            };
            Ok(vec![Value::Int(bits_to_int(res_bits, ty))])
        }
        0x0036 | 0x0037 => {
            // ROTL / ROTR (a, s)
            let ty = val_ty(0)?;
            let w = int_width(ty).map_err(trap)?;
            let a = as_int(&val(0)?, "a")?;
            let s = as_int(&val(1)?, "shift")?;
            let su =
                u32::try_from(s).map_err(|_| trap(format!("{name}: invalid rotate amount {s}")))?;
            let n = u32::from(w);
            let sh = su % n;
            let bits = int_to_bits(a, w);
            let res_bits = if sh == 0 {
                bits
            } else if op.opcode == 0x0036 {
                (bits << sh) | (bits >> (n - sh))
            } else {
                (bits >> sh) | (bits << (n - sh))
            };
            Ok(vec![Value::Int(bits_to_int(res_bits, ty))])
        }

        // 0x0040..0x004B Floating point
        0x0040 => {
            let ty = val_ty(0)?;
            Ok(vec![Value::Float(float_ops::fadd(
                ty,
                as_float(&val(0)?, "a")?,
                as_float(&val(1)?, "b")?,
            ))])
        }
        0x0041 => {
            let ty = val_ty(0)?;
            Ok(vec![Value::Float(float_ops::fsub(
                ty,
                as_float(&val(0)?, "a")?,
                as_float(&val(1)?, "b")?,
            ))])
        }
        0x0042 => {
            let ty = val_ty(0)?;
            Ok(vec![Value::Float(float_ops::fmul(
                ty,
                as_float(&val(0)?, "a")?,
                as_float(&val(1)?, "b")?,
            ))])
        }
        0x0043 => {
            let ty = val_ty(0)?;
            Ok(vec![Value::Float(float_ops::fdiv(
                ty,
                as_float(&val(0)?, "a")?,
                as_float(&val(1)?, "b")?,
            ))])
        }
        0x0044 => {
            let ty = val_ty(0)?;
            Ok(vec![Value::Float(float_ops::fma(
                ty,
                as_float(&val(0)?, "a")?,
                as_float(&val(1)?, "b")?,
                as_float(&val(2)?, "c")?,
            ))])
        }
        0x0045 => {
            let ty = val_ty(0)?;
            Ok(vec![Value::Float(float_ops::fneg(
                ty,
                as_float(&val(0)?, "a")?,
            ))])
        }
        0x0046 => {
            let ty = val_ty(0)?;
            Ok(vec![Value::Float(float_ops::fabs(
                ty,
                as_float(&val(0)?, "a")?,
            ))])
        }
        0x0047 => {
            let ty = val_ty(0)?;
            Ok(vec![Value::Float(float_ops::fmin(
                ty,
                as_float(&val(0)?, "a")?,
                as_float(&val(1)?, "b")?,
            ))])
        }
        0x0048 => {
            let ty = val_ty(0)?;
            Ok(vec![Value::Float(float_ops::fmax(
                ty,
                as_float(&val(0)?, "a")?,
                as_float(&val(1)?, "b")?,
            ))])
        }
        0x0049 => {
            let ty = val_ty(0)?;
            let x = as_float(&val(0)?, "x")?;
            let lo = as_float(&val(1)?, "lo")?;
            let hi = as_float(&val(2)?, "hi")?;
            let res = float_ops::fclamp(ty, x, lo, hi).map_err(|obl| {
                trap(format!(
                    "{obl}({ty}): invalid bounds lo={lo:#x}, hi={hi:#x} (x={x:#x})"
                ))
            })?;
            Ok(vec![Value::Float(res)])
        }
        0x004A => {
            let ty = val_ty(0)?;
            let x = as_float(&val(0)?, "x")?;
            let y = as_float(&val(1)?, "y")?;
            let res = float_ops::fcopysign(ty, x, y)
                .map_err(|obl| trap(format!("{obl}({ty}): y is NaN (x={x:#x}, y={y:#x})")))?;
            Ok(vec![Value::Float(res)])
        }
        0x004B => {
            let ty = val_ty(0)?;
            Ok(vec![Value::Float(float_ops::fsqrt(
                ty,
                as_float(&val(0)?, "a")?,
            ))])
        }

        // 0x0050..0x0058 Compare and logic
        0x0050 | 0x0051 => {
            // EQ / NE
            let ty = val_ty(0)?;
            let a = val(0)?;
            let b = val(1)?;
            let eq = match ty {
                Type::Bool => as_bool(&a, "a")? == as_bool(&b, "b")?,
                t if t.is_int() => as_int(&a, "a")? == as_int(&b, "b")?,
                t if t.is_float() => float_ops::feq(t, as_float(&a, "a")?, as_float(&b, "b")?),
                _ => {
                    return Err(trap(format!("{name}: unsupported comparison type {ty}")));
                }
            };
            Ok(vec![Value::Bool(if op.opcode == 0x0050 {
                eq
            } else {
                !eq
            })])
        }
        0x0052..=0x0055 => {
            // LT, LE, GT, GE .<s|u|f>
            let ty = val_ty(0)?;
            let a = val(0)?;
            let b = val(1)?;
            let res = match (op.mode, ty) {
                (1, Type::SInt(_)) => {
                    let x = as_int(&a, "a")?;
                    let y = as_int(&b, "b")?;
                    match op.opcode {
                        0x0052 => x < y,
                        0x0053 => x <= y,
                        0x0054 => x > y,
                        _ => x >= y,
                    }
                }
                (2, Type::UInt(_)) => {
                    let x = as_int(&a, "a")?;
                    let y = as_int(&b, "b")?;
                    match op.opcode {
                        0x0052 => x < y,
                        0x0053 => x <= y,
                        0x0054 => x > y,
                        _ => x >= y,
                    }
                }
                (3, t) if t.is_float() => {
                    let x = as_float(&a, "a")?;
                    let y = as_float(&b, "b")?;
                    match op.opcode {
                        0x0052 => float_ops::flt(t, x, y),
                        0x0053 => float_ops::fle(t, x, y),
                        0x0054 => float_ops::fgt(t, x, y),
                        _ => float_ops::fge(t, x, y),
                    }
                }
                _ => {
                    return Err(trap(format!(
                        "{name}: mode {} does not match operand type {ty}",
                        op.mode
                    )));
                }
            };
            Ok(vec![Value::Bool(res)])
        }
        0x0056 => Ok(vec![Value::Bool(
            as_bool(&val(0)?, "a")? && as_bool(&val(1)?, "b")?,
        )]),
        0x0057 => Ok(vec![Value::Bool(
            as_bool(&val(0)?, "a")? || as_bool(&val(1)?, "b")?,
        )]),
        0x0058 => Ok(vec![Value::Bool(!as_bool(&val(0)?, "a")?)]),

        // 0x0060..0x0064 Collections
        0x0060 => {
            // INDEX(xs, i)
            let Value::Seq(elems) = val(0)? else {
                return Err(trap("INDEX: expected sequence".to_string()));
            };
            let i = as_int(&val(1)?, "index")?;
            let idx = usize::try_from(i)
                .ok()
                .filter(|&k| k < elems.len())
                .ok_or_else(|| {
                    trap(format!(
                        "INDEX.in_bounds: index {i} out of bounds for length {}",
                        elems.len()
                    ))
                })?;
            Ok(vec![elems[idx].clone()])
        }
        0x0061 => {
            // UPDATE(xs, i, v)
            let Value::Seq(mut elems) = val(0)? else {
                return Err(trap("UPDATE: expected sequence".to_string()));
            };
            let i = as_int(&val(1)?, "index")?;
            let v = val(2)?;
            let idx = usize::try_from(i)
                .ok()
                .filter(|&k| k < elems.len())
                .ok_or_else(|| {
                    trap(format!(
                        "UPDATE.in_bounds: index {i} out of bounds for length {}",
                        elems.len()
                    ))
                })?;
            elems[idx] = v;
            Ok(vec![Value::Seq(elems)])
        }
        0x0062 => {
            // LEN(xs)
            let Value::Seq(elems) = val(0)? else {
                return Err(trap("LEN: expected sequence".to_string()));
            };
            Ok(vec![Value::Int(elems.len() as i128)])
        }
        0x0063 => {
            // SLICE(xs, lo, hi)
            let Value::Seq(elems) = val(0)? else {
                return Err(trap("SLICE: expected sequence".to_string()));
            };
            let lo = as_int(&val(1)?, "lo")?;
            let hi = as_int(&val(2)?, "hi")?;
            if lo < 0 || hi < lo || hi > elems.len() as i128 {
                return Err(trap(format!(
                    "SLICE.in_bounds: range [{lo}, {hi}) out of bounds for length {}",
                    elems.len()
                )));
            }
            Ok(vec![Value::Seq(elems[lo as usize..hi as usize].to_vec())])
        }
        0x0064 => {
            // CONCAT(a, b)
            let Value::Seq(mut a) = val(0)? else {
                return Err(trap("CONCAT: expected sequence for a".to_string()));
            };
            let Value::Seq(b) = val(1)? else {
                return Err(trap("CONCAT: expected sequence for b".to_string()));
            };
            let total = (a.len() as u128) + (b.len() as u128);
            if total >= (1u128 << 64) {
                return Err(trap(format!(
                    "CONCAT.length: combined length {} + {} >= 2^64",
                    a.len(),
                    b.len()
                )));
            }
            a.extend(b);
            Ok(vec![Value::Seq(a)])
        }

        // 0x0100 SIMD_MAP(@f, xs, b...)
        0x0100 => {
            let h = callee_hash(0)?;
            let kernel = callee(&h).ok_or_else(|| {
                trap(format!("SIMD_MAP: missing callee {}", vg_core::ir::hex(&h)))
            })?;
            ensure_kernel(&kernel, op_idx, name)?;
            let Value::Seq(xs) = val(1)? else {
                return Err(trap("SIMD_MAP: expected Array/Tensor/Slice".to_string()));
            };
            let mut bcasts = Vec::with_capacity(op.operands.len().saturating_sub(2));
            for i in 2..op.operands.len() {
                bcasts.push(val(i)?);
            }
            let mut out = Vec::with_capacity(xs.len());
            let mut pure_host = NoEffectsHost;
            for (elem_idx, elem) in xs.into_iter().enumerate() {
                let mut args = Vec::with_capacity(1 + bcasts.len());
                args.push(elem);
                args.extend(bcasts.iter().cloned());
                check_callee_pre(
                    &kernel,
                    &args,
                    callee,
                    op_idx,
                    name,
                    "MAP.pre",
                    Some(elem_idx),
                    depth,
                )?;
                let res = run_inner_with_depth(
                    &kernel,
                    &args,
                    callee,
                    &mut pure_host,
                    &mut None,
                    depth + 1,
                )
                .map_err(|e| trap(format!("SIMD_MAP kernel failed at element {elem_idx}: {e}")))?;
                if res.len() != 1 {
                    return Err(trap(format!(
                        "SIMD_MAP: kernel returned {} results, expected 1",
                        res.len()
                    )));
                }
                out.push(res.into_iter().next().expect("checked len == 1"));
            }
            Ok(vec![Value::Seq(out)])
        }

        // 0x0101 FOLD_BOUNDED(@f, xs, b..., init=a, invariant=@I)
        0x0101 => {
            if op.operands.len() < 4 {
                return Err(trap("FOLD_BOUNDED: fewer than 4 operands".to_string()));
            }
            let h = callee_hash(0)?;
            let kernel = callee(&h).ok_or_else(|| {
                trap(format!(
                    "FOLD_BOUNDED: missing callee {}",
                    vg_core::ir::hex(&h)
                ))
            })?;
            ensure_kernel(&kernel, op_idx, name)?;
            let Value::Seq(xs) = val(1)? else {
                return Err(trap("FOLD_BOUNDED: expected Array or Slice".to_string()));
            };
            let init_slot = op.operands.len() - 2;
            let mut acc = val(init_slot)?;
            let mut bcasts = Vec::with_capacity(init_slot - 2);
            for i in 2..init_slot {
                bcasts.push(val(i)?);
            }
            let mut pure_host = NoEffectsHost;
            for (step, elem) in xs.into_iter().enumerate() {
                let mut args = Vec::with_capacity(2 + bcasts.len());
                args.push(acc);
                args.push(elem);
                args.extend(bcasts.iter().cloned());
                check_callee_pre(
                    &kernel,
                    &args,
                    callee,
                    op_idx,
                    name,
                    "FOLD.pre",
                    Some(step),
                    depth,
                )?;
                let res = run_inner_with_depth(
                    &kernel,
                    &args,
                    callee,
                    &mut pure_host,
                    &mut None,
                    depth + 1,
                )
                .map_err(|e| trap(format!("FOLD_BOUNDED kernel failed at step {step}: {e}")))?;
                if res.len() != 1 {
                    return Err(trap(format!(
                        "FOLD_BOUNDED: kernel returned {} results, expected 1",
                        res.len()
                    )));
                }
                acc = res.into_iter().next().expect("checked len == 1");
            }
            Ok(vec![acc])
        }

        // 0x0105 ASSERT_GUARD(c, @f, a..., else=@g)
        0x0105 => {
            if op.operands.len() < 3 {
                return Err(trap("ASSERT_GUARD: fewer than 3 operands".to_string()));
            }
            let cond = as_bool(&val(0)?, "condition")?;
            let (h, pre_kind) = if cond {
                (callee_hash(1)?, "GUARD.then.pre")
            } else {
                (callee_hash(op.operands.len() - 1)?, "GUARD.else.pre")
            };
            let branch = callee(&h).ok_or_else(|| {
                trap(format!(
                    "ASSERT_GUARD: missing branch callee {}",
                    vg_core::ir::hex(&h)
                ))
            })?;
            if branch.spec {
                return Err(trap(format!(
                    "ASSERT_GUARD: branch {} is a SPEC node",
                    vg_core::ir::hex(&h)
                )));
            }
            let mut args = Vec::with_capacity(op.operands.len() - 3 + branch.learnable.len());
            for i in 2..(op.operands.len() - 1) {
                args.push(val(i)?);
            }
            append_callee_theta(&h, &branch, &mut args, host, op_idx, name)?;
            check_callee_pre(&branch, &args, callee, op_idx, name, pre_kind, None, depth)?;
            run_inner_with_depth(&branch, &args, callee, host, last_clock, depth + 1)
                .map_err(|e| e.with_op(op_idx, name))
        }

        // 0x0107 GENERATE(@f, n, b...)
        0x0107 => {
            let h = callee_hash(0)?;
            let kernel = callee(&h).ok_or_else(|| {
                trap(format!("GENERATE: missing callee {}", vg_core::ir::hex(&h)))
            })?;
            ensure_kernel(&kernel, op_idx, name)?;
            let n_i128 = as_int(&val(1)?, "count")?;
            let n = usize::try_from(n_i128)
                .map_err(|_| trap(format!("GENERATE: invalid count {n_i128}")))?;
            // shortcut: the interpreter allocates at most 2^24 generated elements and traps above,
            // as native code does when the arena is too small; raise it when a test needs more.
            if n > 1 << 24 {
                return Err(trap(format!(
                    "GENERATE: count {n} exceeds the interpreter's limit of 2^24 elements"
                )));
            }
            let mut bcasts = Vec::with_capacity(op.operands.len().saturating_sub(2));
            for i in 2..op.operands.len() {
                bcasts.push(val(i)?);
            }
            let mut out = Vec::with_capacity(n);
            let mut pure_host = NoEffectsHost;
            for i in 0..n {
                let mut args = Vec::with_capacity(1 + bcasts.len());
                args.push(Value::Int(i as i128));
                args.extend(bcasts.iter().cloned());
                check_callee_pre(
                    &kernel,
                    &args,
                    callee,
                    op_idx,
                    name,
                    "GEN.pre",
                    Some(i),
                    depth,
                )?;
                let res = run_inner_with_depth(
                    &kernel,
                    &args,
                    callee,
                    &mut pure_host,
                    &mut None,
                    depth + 1,
                )
                .map_err(|e| trap(format!("GENERATE kernel failed at index {i}: {e}")))?;
                if res.len() != 1 {
                    return Err(trap(format!(
                        "GENERATE: kernel returned {} results, expected 1",
                        res.len()
                    )));
                }
                out.push(res.into_iter().next().expect("checked len == 1"));
            }
            Ok(vec![Value::Seq(out)])
        }

        // 0x0300 CALL_HASH(@f, a...)
        0x0300 => {
            let h = callee_hash(0)?;
            let target = callee(&h).ok_or_else(|| {
                trap(format!(
                    "CALL_HASH: missing callee {}",
                    vg_core::ir::hex(&h)
                ))
            })?;
            if target.spec {
                return Err(trap(format!(
                    "CALL_HASH: callee {} is a SPEC node",
                    vg_core::ir::hex(&h)
                )));
            }
            let mut args = Vec::with_capacity(op.operands.len() - 1 + target.learnable.len());
            for i in 1..op.operands.len() {
                args.push(val(i)?);
            }
            append_callee_theta(&h, &target, &mut args, host, op_idx, name)?;
            check_callee_pre(
                &target, &args, callee, op_idx, name, "CALL.pre", None, depth,
            )?;
            run_inner_with_depth(&target, &args, callee, host, last_clock, depth + 1)
                .map_err(|e| e.with_op(op_idx, name))
        }

        // 0x0400..0x0402 Effects
        0x0400 => {
            // FILE_READ(e, h, max)
            let Value::Eff = val(0)? else {
                return Err(trap("FILE_READ: operand 0 must be Eff".to_string()));
            };
            let Value::Handle(hid) = val(1)? else {
                return Err(trap("FILE_READ: operand 1 must be Handle".to_string()));
            };
            let max_i = as_int(&val(2)?, "max")?;
            let max = u64::try_from(max_i)
                .map_err(|_| trap(format!("FILE_READ: invalid max {max_i}")))?;
            let res = host
                .file_read(hid, max)
                .map_err(|e| e.with_op(op_idx, name))?;
            let out = match res {
                Ok(bytes) => {
                    if bytes.len() as u64 > max {
                        return Err(trap(format!(
                            "FILE_READ(handle={hid}, max={max}): host returned {} bytes > max",
                            bytes.len()
                        )));
                    }
                    Value::Ok(Box::new(Value::Seq(
                        bytes
                            .into_iter()
                            .map(|b| Value::Int(i128::from(b)))
                            .collect(),
                    )))
                }
                Err(code) => Value::Err(Box::new(Value::Int(i128::from(code)))),
            };
            Ok(vec![Value::Eff, out])
        }
        0x0401 => {
            // FILE_WRITE(e, h, data)
            let Value::Eff = val(0)? else {
                return Err(trap("FILE_WRITE: operand 0 must be Eff".to_string()));
            };
            let Value::Handle(hid) = val(1)? else {
                return Err(trap("FILE_WRITE: operand 1 must be Handle".to_string()));
            };
            let Value::Seq(xs) = val(2)? else {
                return Err(trap("FILE_WRITE: operand 2 must be Slice<u8>".to_string()));
            };
            let mut bytes = Vec::with_capacity(xs.len());
            for x in xs {
                let b = as_int(&x, "byte")?;
                let bu8 = u8::try_from(b)
                    .map_err(|_| trap(format!("FILE_WRITE: invalid byte value {b}")))?;
                bytes.push(bu8);
            }
            let res = host
                .file_write(hid, &bytes)
                .map_err(|e| e.with_op(op_idx, name))?;
            let out = match res {
                Ok(written) => {
                    if written != bytes.len() as u64 {
                        return Err(trap(format!(
                            "FILE_WRITE(handle={hid}): host wrote {written} bytes, expected {}",
                            bytes.len()
                        )));
                    }
                    Value::Ok(Box::new(Value::Int(i128::from(written))))
                }
                Err(code) => Value::Err(Box::new(Value::Int(i128::from(code)))),
            };
            Ok(vec![Value::Eff, out])
        }
        0x0402 => {
            // CLOCK_READ(e, c)
            let Value::Eff = val(0)? else {
                return Err(trap("CLOCK_READ: operand 0 must be Eff".to_string()));
            };
            let Value::Handle(cid) = val(1)? else {
                return Err(trap("CLOCK_READ: operand 1 must be Handle".to_string()));
            };
            let ns = host.clock_read(cid).map_err(|e| e.with_op(op_idx, name))?;
            if let Some(prev) = *last_clock
                && ns < prev
            {
                return Err(trap(format!(
                    "CLOCK_READ(handle={cid}): non-monotonic clock {ns} < {prev}"
                )));
            }
            *last_clock = Some(ns);
            Ok(vec![Value::Eff, Value::Int(i128::from(ns))])
        }

        _ => Err(trap(format!(
            "{name} (opcode {:#06x}): unsupported or non-MVP opcode",
            op.opcode
        ))),
    }
}

trait IntoIntExt {
    fn into_int(self) -> Result<i128, Trap>;
}

impl IntoIntExt for Value {
    fn into_int(self) -> Result<i128, Trap> {
        match self {
            Value::Int(x) => Ok(x),
            other => Err(Trap::entry(format!("expected Int, got {other:?}"))),
        }
    }
}

fn ensure_kernel(kernel: &Node, op_idx: u32, op_name: &'static str) -> Result<(), Trap> {
    if kernel.spec {
        return Err(Trap::op(
            op_idx,
            op_name,
            format!("{op_name}: kernel cannot be a SPEC node"),
        ));
    }
    if kernel.effects.0 != 0 {
        return Err(Trap::op(
            op_idx,
            op_name,
            format!(
                "{op_name}: kernel must be pure, got effects {:?}",
                kernel.effects.names()
            ),
        ));
    }
    Ok(())
}

fn append_callee_theta(
    callee_hash: &Hash,
    callee_node: &Node,
    args: &mut Vec<Value>,
    host: &mut dyn Host,
    op_idx: u32,
    op_name: &'static str,
) -> Result<(), Trap> {
    if callee_node.learnable.is_empty() || args.len() != callee_node.params.len() {
        return Ok(());
    }
    let thetas = if let Some(provided) = host.theta(callee_hash, callee_node) {
        if provided.len() != callee_node.learnable.len() {
            return Err(Trap::op(
                op_idx,
                op_name,
                format!(
                    "host provided {} learnable parameters for {}, expected {}",
                    provided.len(),
                    vg_core::ir::hex(callee_hash),
                    callee_node.learnable.len()
                ),
            ));
        }
        provided
    } else {
        let mut defaults = Vec::with_capacity(callee_node.learnable.len());
        for l in &callee_node.learnable {
            let v = decode_const(&callee_node.consts[l.lo as usize])
                .map_err(|e| Trap::op(op_idx, op_name, format!("learnable bound decode: {e}")))?;
            defaults.push(v);
        }
        defaults
    };
    for (j, (l, v)) in callee_node.learnable.iter().zip(thetas).enumerate() {
        let canon = validate_and_canonicalize(&l.ty, &v).map_err(|e| {
            Trap::op(
                op_idx,
                op_name,
                format!("learnable theta #{j} ({}): {e}", l.ty),
            )
        })?;
        let Value::Float(bits) = canon else {
            return Err(Trap::op(
                op_idx,
                op_name,
                format!("learnable theta #{j}: expected float"),
            ));
        };
        let lo_val = decode_const(&callee_node.consts[l.lo as usize])
            .map_err(|e| Trap::op(op_idx, op_name, format!("learnable lo decode: {e}")))?;
        let hi_val = decode_const(&callee_node.consts[l.hi as usize])
            .map_err(|e| Trap::op(op_idx, op_name, format!("learnable hi decode: {e}")))?;
        let (Value::Float(lo_bits), Value::Float(hi_bits)) = (lo_val, hi_val) else {
            return Err(Trap::op(
                op_idx,
                op_name,
                "learnable bounds must be floats".to_string(),
            ));
        };
        if !float_ops::fge(&l.ty, bits, lo_bits) || !float_ops::fle(&l.ty, bits, hi_bits) {
            return Err(Trap::op(
                op_idx,
                op_name,
                format!("learnable theta #{j} out of declared box [{lo_bits:#x}, {hi_bits:#x}]"),
            ));
        }
        args.push(canon);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn check_callee_pre(
    callee_node: &Node,
    args: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
    op_idx: u32,
    op_name: &'static str,
    obl_kind: &'static str,
    step: Option<usize>,
    depth: usize,
) -> Result<(), Trap> {
    if callee_node.contract.pre.is_empty() {
        return Ok(());
    }
    let ok = holds_with_depth(callee_node, Clause::Pre, args, &[], callee, depth + 1)
        .map_err(|e| e.with_op(op_idx, op_name))?;
    if !ok {
        let suffix = step.map_or_else(String::new, |s| format!(" at step/elem {s}"));
        return Err(Trap::op(
            op_idx,
            op_name,
            format!("{obl_kind}{suffix}: callee PRE failed for args {args:?}"),
        ));
    }
    Ok(())
}

fn int_width(ty: &Type) -> Result<u8, String> {
    match ty {
        Type::UInt(w) | Type::SInt(w) if matches!(w, 8 | 16 | 32 | 64) => Ok(*w),
        _ => Err(format!("expected integer type, got {ty}")),
    }
}

fn int_bounds(ty: &Type) -> Result<(i128, i128), String> {
    match ty {
        Type::UInt(w) => Ok((0, (1i128 << *w) - 1)),
        Type::SInt(w) => Ok((-(1i128 << (*w - 1)), (1i128 << (*w - 1)) - 1)),
        _ => Err(format!("expected integer type, got {ty}")),
    }
}

fn wrap_int(v: i128, ty: &Type) -> Result<i128, String> {
    match ty {
        Type::UInt(w) => {
            let mask = if *w == 64 {
                u64::MAX as u128
            } else {
                (1u128 << *w) - 1
            };
            Ok((v as u128 & mask) as i128)
        }
        Type::SInt(w) => {
            let shift = 128 - u32::from(*w);
            Ok((v << shift) >> shift)
        }
        _ => Err(format!("expected integer type, got {ty}")),
    }
}

fn int_arith_result(
    mode: u8,
    op_name: &str,
    ty: &Type,
    a: i128,
    b: i128,
    exact: i128,
) -> Result<Vec<Value>, String> {
    let (lo, hi) = int_bounds(ty)?;
    let out = match mode {
        0 => {
            if !int_fits(exact, ty) {
                return Err(format!(
                    "{op_name}.no_overflow({ty}): ({a}, {b}) -> {exact} out of range [{lo}, {hi}]"
                ));
            }
            exact
        }
        1 => wrap_int(exact, ty)?,
        2 => exact.clamp(lo, hi),
        m => return Err(format!("{op_name}: unknown mode {m}")),
    };
    Ok(vec![Value::Int(out)])
}

fn int_to_bits(v: i128, w: u8) -> u64 {
    let mask = if w == 64 { u64::MAX } else { (1u64 << w) - 1 };
    (v as u64) & mask
}

fn bits_to_int(bits: u64, ty: &Type) -> i128 {
    match ty {
        Type::UInt(w) => {
            let mask = if *w == 64 { u64::MAX } else { (1u64 << *w) - 1 };
            i128::from(bits & mask)
        }
        Type::SInt(w) => {
            let sh = 64 - u32::from(*w);
            i128::from(((bits as i64) << sh) >> sh)
        }
        _ => unreachable!("int type checked"),
    }
}

fn bitwise_bin(
    ty: &Type,
    a: i128,
    b: i128,
    f: impl FnOnce(u64, u64) -> u64,
) -> Result<Vec<Value>, String> {
    let w = int_width(ty)?;
    let res = f(int_to_bits(a, w), int_to_bits(b, w));
    Ok(vec![Value::Int(bits_to_int(res, ty))])
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use vg_core::asm_local;

    use super::*;
    use crate::host::{RecordingHost, TraceHost};

    fn run_last(src: &str, inputs: &[Value], host: &mut dyn Host) -> Result<Vec<Value>, Trap> {
        let assembled = asm_local(src).expect("valid VG-Sym");
        let by_hash: HashMap<Hash, Node> = assembled
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let target = &assembled.last().expect("at least one node").node;
        run(target, inputs, &|h| by_hash.get(h).cloned(), host)
    }

    #[test]
    fn int_modes_and_traps() {
        let src = r#"
module m;
NODE @ops {
  SIGNATURE: (a: i8, b: i8) -> (i8, i8, i8);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = ADD.checked(a, b);
    %1 = ADD.wrap(a, b);
    %2 = ADD.sat(a, b);
    RETURN(%0, %1, %2);
  }
}
"#;
        let mut h = NoEffectsHost;
        assert_eq!(
            run_last(src, &[Value::Int(10), Value::Int(20)], &mut h),
            Ok(vec![Value::Int(30), Value::Int(30), Value::Int(30)])
        );
        let trap = run_last(src, &[Value::Int(127), Value::Int(1)], &mut h).unwrap_err();
        assert_eq!(trap.op, Some(0));
        assert_eq!(trap.opcode, Some("ADD"));
        assert!(trap.message.contains("ADD.no_overflow"));
        assert!(trap.message.contains("127"));

        let src_div = r#"
module m;
NODE @d {
  SIGNATURE: (a: i8, b: i8) -> (i8, i8, i8, i8);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = DIV.wrap(a, b);
    %1 = DIV.sat(a, b);
    %2 = MOD.checked(a, b);
    %3 = NEG.sat(a);
    RETURN(%0, %1, %2, %3);
  }
}
"#;
        assert_eq!(
            run_last(src_div, &[Value::Int(-128), Value::Int(-1)], &mut h),
            Ok(vec![
                Value::Int(-128),
                Value::Int(127),
                Value::Int(0),
                Value::Int(127)
            ])
        );
        let div0 = run_last(src_div, &[Value::Int(5), Value::Int(0)], &mut h).unwrap_err();
        assert!(div0.message.contains("DIV.nonzero"));
    }

    #[test]
    fn u64_mul_checked_wrap_sat() {
        let src = r#"
module m;
NODE @mul_u64 {
  SIGNATURE: (a: u64, b: u64) -> (u64, u64);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = MUL.wrap(a, b);
    %1 = MUL.sat(a, b);
    RETURN(%0, %1);
  }
}
"#;
        let mut h = NoEffectsHost;
        let max = i128::from(u64::MAX);
        // (2^64 - 1)^2 mod 2^64 = 1
        assert_eq!(
            run_last(src, &[Value::Int(max), Value::Int(max)], &mut h),
            Ok(vec![Value::Int(1), Value::Int(max)])
        );
    }

    #[test]
    fn strict_vs_lazy_assert_guard() {
        // SELECT evaluates both branches strictly, so division by 0 traps even when condition is true.
        let strict_src = r#"
module m;
NODE @bad_select {
  SIGNATURE: (c: Bool, x: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = DIV.checked(x, 0:i32);
    %1 = SELECT(c, x, %0);
    RETURN(%1);
  }
}
"#;
        let mut h = NoEffectsHost;
        let t = run_last(strict_src, &[Value::Bool(true), Value::Int(7)], &mut h).unwrap_err();
        assert_eq!(t.opcode, Some("DIV"));

        // ASSERT_GUARD only evaluates the taken branch.
        let guard_src = r#"
module m;
NODE @safe_branch {
  SIGNATURE: (x: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    RETURN(x);
  }
}
NODE @trapping_branch {
  SIGNATURE: (x: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = DIV.checked(x, 0:i32);
    RETURN(%0);
  }
}
NODE @lazy_guard {
  SIGNATURE: (c: Bool, x: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = ASSERT_GUARD.branch(c, @safe_branch, x, else=@trapping_branch);
    RETURN(%0);
  }
}
"#;
        assert_eq!(
            run_last(guard_src, &[Value::Bool(true), Value::Int(7)], &mut h),
            Ok(vec![Value::Int(7)])
        );
        let err = run_last(guard_src, &[Value::Bool(false), Value::Int(7)], &mut h).unwrap_err();
        assert!(err.message.contains("DIV.nonzero"));
    }

    #[test]
    fn effect_trace_and_monotonic_clock() {
        let src = r#"
module m;
NODE @io {
  SIGNATURE: (e: Eff, clk: Clock, fin: FileIn, fout: FileOut) -> (Eff, u64, Result<u64, u32>);
  EFFECTS: [ARENA, CLOCK, FILE_READ, FILE_WRITE];
  CONTRACT {}
  BODY {
    %0, %1 = CLOCK_READ(e, clk);
    %2, %3 = FILE_READ(%0, fin, 4:u64);
    %4 = GET_OK(%3);
    %5, %6 = FILE_WRITE(%2, fout, %4);
    %7, %8 = CLOCK_READ(%5, clk);
    %9 = SUB.checked(%8, %1);
    RETURN(%7, %9, %6);
  }
}
"#;
        let trace_json = r#"[
            {"op": "CLOCK_READ", "result": "100"},
            {"op": "FILE_READ", "result": {"ok": [104, 105]}},
            {"op": "FILE_WRITE", "result": {"ok": "2"}},
            {"op": "CLOCK_READ", "result": "155"}
        ]"#;
        let host = TraceHost::from_json(trace_json).unwrap();
        let mut rec = RecordingHost::new(host);
        let out = run_last(
            src,
            &[
                Value::Eff,
                Value::Handle(0),
                Value::Handle(1),
                Value::Handle(2),
            ],
            &mut rec,
        )
        .unwrap();
        assert_eq!(
            out,
            vec![
                Value::Eff,
                Value::Int(55),
                Value::Ok(Box::new(Value::Int(2))),
            ]
        );
        assert!(rec.inner().is_exhausted());

        // Non-monotonic clock traps.
        let bad_clock = r#"[
            {"op": "CLOCK_READ", "result": "200"},
            {"op": "FILE_READ", "result": {"ok": [1]}},
            {"op": "FILE_WRITE", "result": {"ok": "1"}},
            {"op": "CLOCK_READ", "result": "199"}
        ]"#;
        let mut bad_host = TraceHost::from_json(bad_clock).unwrap();
        let err = run_last(
            src,
            &[
                Value::Eff,
                Value::Handle(0),
                Value::Handle(1),
                Value::Handle(2),
            ],
            &mut bad_host,
        )
        .unwrap_err();
        assert!(err.message.contains("non-monotonic"));
    }
}
