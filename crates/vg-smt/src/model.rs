//! Small-model search (ADR 0006) and Z3 model decoding into `vg_core::value::Value`.

use vg_core::float;
use vg_core::ir::Node;
use vg_core::types::Type;
use vg_core::value::Value;

use crate::sexpr::SExpr;
use crate::solver::{CheckResult, SmtQuery, Solver};
use crate::sort::{SymVal, bv_lit_u128, flatten_type, fp_format, symval_from_atoms};

const SMALL_SLICE_CAPS: [u64; 3] = [4, 16, 256];
const MAX_SLICE_LEN: u64 = 256;
const MAX_STATIC_ELEMS: u64 = 16_384;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodedModel {
    Values(Vec<Value>),
    TooLarge(String),
    Error(String),
}

pub fn has_slice_type(ty: &Type) -> bool {
    match ty {
        Type::Slice(_) => true,
        Type::Array(t, _) | Type::Tensor(t, _) => has_slice_type(t),
        Type::Tuple(ms) => ms.iter().any(has_slice_type),
        Type::Result(a, b) => has_slice_type(a) || has_slice_type(b),
        _ => false,
    }
}

pub fn collect_input_slice_lens(sym: &SymVal, out: &mut Vec<String>) {
    match sym {
        SymVal::Slice { len, .. } => out.push(len.clone()),
        SymVal::Tuple { elems } => {
            for (_, v) in elems {
                collect_input_slice_lens(v, out);
            }
        }
        SymVal::Result { ok, err, .. } => {
            collect_input_slice_lens(ok, out);
            collect_input_slice_lens(err, out);
        }
        _ => {}
    }
}

/// Runs small-model search (caps 4, 16, 256) when any input has a Slice, then decodes inputs.
pub fn decode_with_small_model(
    solver: &mut dyn Solver,
    base_query: &SmtQuery,
    node: &Node,
    input_syms: &[SymVal],
) -> DecodedModel {
    let needs_cap = node.params.iter().any(has_slice_type);
    if needs_cap {
        let mut slice_lens = Vec::new();
        for s in input_syms {
            collect_input_slice_lens(s, &mut slice_lens);
        }
        if !slice_lens.is_empty() {
            let mut found_small_sat = false;
            for cap in SMALL_SLICE_CAPS {
                let mut bounded = base_query.clone();
                let cap_lit = bv_lit_u128(u128::from(cap), 64);
                for l in &slice_lens {
                    bounded.assertions.push(format!("(bvule {l} {cap_lit})"));
                }
                if matches!(solver.check_query(&bounded), CheckResult::Sat) {
                    found_small_sat = true;
                    break;
                }
            }
            if !found_small_sat {
                // Re-establish the original model if bounded searches failed/timed out.
                match solver.check_query(base_query) {
                    CheckResult::Sat => {}
                    CheckResult::Unknown { reason, .. } => {
                        return DecodedModel::TooLarge(format!(
                            "no small slice model found: {reason}"
                        ));
                    }
                    CheckResult::Unsat => {
                        return DecodedModel::Error(
                            "query became unsat during small-model search".to_string(),
                        );
                    }
                    CheckResult::Error(e) => return DecodedModel::Error(e),
                }
            }
        }
    }
    decode_inputs(solver, node, input_syms)
}

/// Decodes every node input (`params` then `learnable`) from the solver's active SAT model.
pub fn decode_inputs(solver: &mut dyn Solver, node: &Node, input_syms: &[SymVal]) -> DecodedModel {
    let mut out = Vec::with_capacity(node.input_count());
    let mut next_coll_id = 0x1000_0000u32;
    for (i, sym) in input_syms.iter().enumerate() {
        let ty = node.input_type(i as u32).expect("input in range");
        match decode_symval(solver, ty, sym, &mut next_coll_id) {
            Ok(v) => out.push(v),
            Err(DecodeErr::TooLarge(msg)) => return DecodedModel::TooLarge(msg),
            Err(DecodeErr::Solver(msg)) => return DecodedModel::Error(msg),
        }
    }
    DecodedModel::Values(out)
}

enum DecodeErr {
    TooLarge(String),
    Solver(String),
}

fn decode_symval(
    solver: &mut dyn Solver,
    ty: &Type,
    sym: &SymVal,
    next_coll_id: &mut u32,
) -> Result<Value, DecodeErr> {
    match (ty, sym) {
        (Type::Eff, _) => Ok(Value::Eff),
        (Type::Bool, SymVal::Bool(e)) => {
            let vals = solver
                .get_values(std::slice::from_ref(e))
                .map_err(DecodeErr::Solver)?;
            parse_bool(&vals[0])
                .map(Value::Bool)
                .map_err(DecodeErr::Solver)
        }
        (Type::UInt(_) | Type::SInt(_), SymVal::Int { expr, .. }) => {
            let vals = solver
                .get_values(std::slice::from_ref(expr))
                .map_err(DecodeErr::Solver)?;
            parse_int(&vals[0], ty)
                .map(Value::Int)
                .map_err(DecodeErr::Solver)
        }
        (Type::Float(_) | Type::BFloat16, SymVal::Float { expr, .. }) => {
            let vals = solver
                .get_values(std::slice::from_ref(expr))
                .map_err(DecodeErr::Solver)?;
            parse_float(&vals[0], ty)
                .map(Value::Float)
                .map_err(DecodeErr::Solver)
        }
        (Type::Handle(_), SymVal::Handle(expr)) => {
            let vals = solver
                .get_values(std::slice::from_ref(expr))
                .map_err(DecodeErr::Solver)?;
            let raw = parse_bv_u128(&vals[0]).map_err(DecodeErr::Solver)?;
            Ok(Value::Handle(raw as u32))
        }
        (Type::Tuple(ts), SymVal::Tuple { elems }) => {
            let mut vs = Vec::with_capacity(ts.len());
            for (t, (_, s)) in ts.iter().zip(elems) {
                vs.push(decode_symval(solver, t, s, next_coll_id)?);
            }
            Ok(Value::Tuple(vs))
        }
        (Type::Result(ok_ty, err_ty), SymVal::Result { is_ok, ok, err, .. }) => {
            let vals = solver
                .get_values(std::slice::from_ref(is_ok))
                .map_err(DecodeErr::Solver)?;
            let tag = parse_bool(&vals[0]).map_err(DecodeErr::Solver)?;
            if tag {
                let inner = decode_symval(solver, ok_ty, ok, next_coll_id)?;
                Ok(Value::Ok(Box::new(inner)))
            } else {
                let inner = decode_symval(solver, err_ty, err, next_coll_id)?;
                Ok(Value::Err(Box::new(inner)))
            }
        }
        (
            Type::Array(_, _),
            SymVal::Array {
                len,
                elem_ty,
                arrays,
                ..
            },
        ) => decode_collection(
            solver,
            *len,
            MAX_STATIC_ELEMS,
            elem_ty,
            arrays,
            next_coll_id,
        ),
        (
            Type::Tensor(_, dims),
            SymVal::Tensor {
                elem_ty, arrays, ..
            },
        ) => {
            let total = dims
                .iter()
                .fold(1u64, |acc, &d| acc.saturating_mul(u64::from(d)));
            decode_collection(
                solver,
                total,
                MAX_STATIC_ELEMS,
                elem_ty,
                arrays,
                next_coll_id,
            )
        }
        (
            Type::Slice(_),
            SymVal::Slice {
                len,
                elem_ty,
                arrays,
                ..
            },
        ) => {
            let vals = solver
                .get_values(std::slice::from_ref(len))
                .map_err(DecodeErr::Solver)?;
            let n = parse_bv_u128(&vals[0]).map_err(DecodeErr::Solver)?;
            let n_u64 = u64::try_from(n)
                .map_err(|_| DecodeErr::TooLarge(format!("slice length {n} exceeds u64 range")))?;
            decode_collection(solver, n_u64, MAX_SLICE_LEN, elem_ty, arrays, next_coll_id)
        }
        _ => Err(DecodeErr::Solver(format!(
            "input type {ty} does not match symbolic value {sym:?}"
        ))),
    }
}

fn decode_collection(
    solver: &mut dyn Solver,
    count: u64,
    cap: u64,
    elem_ty: &Type,
    arrays: &[String],
    next_coll_id: &mut u32,
) -> Result<Value, DecodeErr> {
    if count > cap {
        return Err(DecodeErr::TooLarge(format!(
            "collection length {count} exceeds replay cap {cap}"
        )));
    }
    let count = count as usize;
    if count == 0 {
        return Ok(Value::Seq(Vec::new()));
    }
    let k = flatten_type(elem_ty).len();
    debug_assert_eq!(k, arrays.len());
    if k == 0 {
        // Element type has no SMT atoms (should not occur for collections).
        return Ok(Value::Seq(Vec::new()));
    }

    // Batch primitive leaf lookups directly when element type is a scalar/Bool/Handle;
    // for nested tuples/results/collections, build SymVal element handles and decode recursively.
    let mut queries = Vec::with_capacity(count * k);
    for idx in 0..count {
        let idx_lit = bv_lit_u128(idx as u128, 64);
        for arr in arrays {
            queries.push(format!("(select {arr} {idx_lit})"));
        }
    }
    if is_primitive_elem(elem_ty) {
        let vals = solver.get_values(&queries).map_err(DecodeErr::Solver)?;
        let mut elems = Vec::with_capacity(count);
        for raw in vals {
            elems.push(decode_primitive(&raw, elem_ty).map_err(DecodeErr::Solver)?);
        }
        Ok(Value::Seq(elems))
    } else {
        let mut elems = Vec::with_capacity(count);
        for idx in 0..count {
            let chunk = &queries[idx * k..(idx + 1) * k];
            let sym = symval_from_atoms(elem_ty, chunk, next_coll_id);
            elems.push(decode_symval(solver, elem_ty, &sym, next_coll_id)?);
        }
        Ok(Value::Seq(elems))
    }
}

fn is_primitive_elem(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Bool
            | Type::UInt(_)
            | Type::SInt(_)
            | Type::Float(_)
            | Type::BFloat16
            | Type::Handle(_)
    )
}

fn decode_primitive(sexpr: &SExpr, ty: &Type) -> Result<Value, String> {
    match ty {
        Type::Bool => parse_bool(sexpr).map(Value::Bool),
        Type::UInt(_) | Type::SInt(_) => parse_int(sexpr, ty).map(Value::Int),
        Type::Float(_) | Type::BFloat16 => parse_float(sexpr, ty).map(Value::Float),
        Type::Handle(_) => parse_bv_u128(sexpr).map(|v| Value::Handle(v as u32)),
        _ => Err(format!("non-primitive element type {ty}")),
    }
}

pub fn parse_bool(sexpr: &SExpr) -> Result<bool, String> {
    match sexpr.as_atom() {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(format!("expected Bool, got {sexpr}")),
    }
}

pub fn parse_bv_u128(sexpr: &SExpr) -> Result<u128, String> {
    match sexpr {
        SExpr::Atom(s) => {
            if let Some(hex) = s.strip_prefix("#x") {
                u128::from_str_radix(hex, 16).map_err(|e| format!("invalid hex BV {s}: {e}"))
            } else if let Some(bin) = s.strip_prefix("#b") {
                u128::from_str_radix(bin, 2).map_err(|e| format!("invalid bin BV {s}: {e}"))
            } else {
                Err(format!("expected bitvector atom, got {s}"))
            }
        }
        SExpr::List(xs) if xs.len() == 3 && xs[0].as_atom() == Some("_") => {
            let bv = xs[1]
                .as_atom()
                .and_then(|a| a.strip_prefix("bv"))
                .ok_or_else(|| format!("expected (_ bvN W), got {sexpr}"))?;
            bv.parse::<u128>()
                .map_err(|e| format!("invalid decimal BV {bv}: {e}"))
        }
        _ => Err(format!("expected bitvector SExpr, got {sexpr}")),
    }
}

pub fn parse_int(sexpr: &SExpr, ty: &Type) -> Result<i128, String> {
    let raw = parse_bv_u128(sexpr)?;
    match ty {
        Type::UInt(w) => {
            let w = u32::from(*w);
            let mask = if w == 128 {
                u128::MAX
            } else {
                (1u128 << w) - 1
            };
            Ok((raw & mask) as i128)
        }
        Type::SInt(w) => {
            let w = u32::from(*w);
            let shift = 128 - w;
            Ok(((raw as i128) << shift) >> shift)
        }
        _ => Err(format!("expected integer type, got {ty}")),
    }
}

pub fn parse_float(sexpr: &SExpr, ty: &Type) -> Result<u64, String> {
    let (eb, sb) = fp_format(ty).ok_or_else(|| format!("expected float type, got {ty}"))?;
    let sig_bits = sb - 1;
    let total = eb + sb;
    let raw = match sexpr {
        SExpr::List(xs) if xs.len() == 4 && xs[0].as_atom() == Some("_") => match xs[1].as_atom() {
            Some("NaN") => float::canonical_nan(ty),
            Some("+zero") => 0,
            Some("-zero") => 1u64 << (total - 1),
            Some("+oo") => ((1u64 << eb) - 1) << sig_bits,
            Some("-oo") => (1u64 << (total - 1)) | (((1u64 << eb) - 1) << sig_bits),
            _ => return Err(format!("unexpected FP special constant: {sexpr}")),
        },
        SExpr::List(xs) if xs.len() == 4 && xs[0].as_atom() == Some("fp") => {
            let s = parse_bv_u128(&xs[1])? as u64;
            let e = parse_bv_u128(&xs[2])? as u64;
            let m = parse_bv_u128(&xs[3])? as u64;
            ((s & 1) << (total - 1))
                | ((e & ((1u64 << eb) - 1)) << sig_bits)
                | (m & ((1u64 << sig_bits) - 1))
        }
        _ => return Err(format!("expected SMT-LIB2 float, got {sexpr}")),
    };
    if float::is_nan(ty, raw) {
        Ok(float::canonical_nan(ty))
    } else {
        Ok(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexpr::parse_sexpr;

    #[test]
    fn decodes_z3_float_variants() {
        let cases = [
            ("(_ NaN 8 24)", Type::Float(32), 0x7FC0_0000u64),
            ("(_ -zero 11 53)", Type::Float(64), 0x8000_0000_0000_0000),
            ("(_ +zero 8 24)", Type::Float(32), 0),
            ("(_ -oo 8 24)", Type::Float(32), 0xFF80_0000),
            ("(_ +oo 5 11)", Type::Float(16), 0x7C00),
            (
                "(fp #b0 #x7f #b00000000000000000000000)",
                Type::Float(32),
                0x3F80_0000,
            ),
            ("(fp #b0 #b01111 #b1000000000)", Type::Float(16), 0x3E00),
            ("(fp #b0 #x7f #b0000000)", Type::BFloat16, 0x3F80),
            // Non-canonical NaN payload from solver must canonicalize per ADR 0002.
            (
                "(fp #b1 #xff #b00000000000000000000001)",
                Type::Float(32),
                0x7FC0_0000,
            ),
        ];
        for (txt, ty, want) in cases {
            let sexpr = parse_sexpr(txt).unwrap();
            assert_eq!(parse_float(&sexpr, &ty).unwrap(), want, "{txt}");
        }
    }

    #[test]
    fn decodes_signed_and_unsigned_ints() {
        assert_eq!(
            parse_int(&parse_sexpr("#x80").unwrap(), &Type::SInt(8)).unwrap(),
            -128
        );
        assert_eq!(
            parse_int(&parse_sexpr("#xff").unwrap(), &Type::UInt(8)).unwrap(),
            255
        );
        assert_eq!(
            parse_int(&parse_sexpr("#x8000000000000000").unwrap(), &Type::SInt(64)).unwrap(),
            i128::from(i64::MIN)
        );
        assert_eq!(
            parse_int(&parse_sexpr("#xffffffffffffffff").unwrap(), &Type::UInt(64)).unwrap(),
            i128::from(u64::MAX)
        );
        assert_eq!(
            parse_int(&parse_sexpr("(_ bv254 8)").unwrap(), &Type::SInt(8)).unwrap(),
            -2
        );
    }
}
