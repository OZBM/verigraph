//! SMT-LIB2 sort mapping, symbolic value flattening, constants, and float helpers.

use std::collections::BTreeMap;
use std::fmt;

use vg_core::float;
use vg_core::ir::Const;
use vg_core::types::Type;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SmtSort {
    Bool,
    BitVec(u32),
    Fp(u32, u32),
    Array(Box<SmtSort>, Box<SmtSort>),
}

impl fmt::Display for SmtSort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmtSort::Bool => f.write_str("Bool"),
            SmtSort::BitVec(w) => write!(f, "(_ BitVec {w})"),
            SmtSort::Fp(eb, sb) => write!(f, "(_ FloatingPoint {eb} {sb})"),
            SmtSort::Array(idx, elem) => write!(f, "(Array {idx} {elem})"),
        }
    }
}

/// Returns `(exponent_bits, significand_bits_including_implicit)` for an MVP float type.
pub fn fp_format(ty: &Type) -> Option<(u32, u32)> {
    match ty {
        Type::Float(16) => Some((5, 11)),
        Type::BFloat16 => Some((8, 8)),
        Type::Float(32) => Some((8, 24)),
        Type::Float(64) => Some((11, 53)),
        _ => None,
    }
}

pub fn fp_bit_width(ty: &Type) -> Option<u32> {
    fp_format(ty).map(|(eb, sb)| eb + sb)
}

/// Canonical NaN pattern of `ty` as an unsigned bitvector literal.
pub fn canonical_nan_bv(ty: &Type) -> String {
    let w = fp_bit_width(ty).expect("float type");
    let bits = float::canonical_nan(ty);
    bv_lit_u128(u128::from(bits), w)
}

/// Bitvector literal for an unsigned value.
pub fn bv_lit_u128(v: u128, width: u32) -> String {
    debug_assert!((1..=128).contains(&width));
    let masked = if width == 128 {
        v
    } else {
        v & ((1u128 << width) - 1)
    };
    if width.is_multiple_of(4) && width <= 64 {
        let digits = (width / 4) as usize;
        format!("#x{masked:0digits$x}")
    } else {
        format!("(_ bv{masked} {width})")
    }
}

/// Signed two's-complement bitvector literal for `v` at `width` bits.
pub fn bv_lit_i128(v: i128, width: u32) -> String {
    debug_assert!((1..=128).contains(&width));
    let mask = if width == 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    };
    bv_lit_u128((v as u128) & mask, width)
}

/// SMT-LIB2 floating-point literal from raw IEEE bits (canonical NaN yields `(_ NaN eb sb)`).
pub fn fp_lit(ty: &Type, bits: u64) -> String {
    let (eb, sb) = fp_format(ty).expect("float type");
    let total = eb + sb;
    if float::is_nan(ty, bits) {
        format!("(_ NaN {eb} {sb})")
    } else {
        format!(
            "((_ to_fp {eb} {sb}) {})",
            bv_lit_u128(u128::from(bits), total)
        )
    }
}

/// Converts a small non-negative integer power-of-2 or integer value into IEEE bits for `ty`.
pub fn int_to_fp_bits(ty: &Type, mag: u128, neg: bool) -> Option<u64> {
    float::parse(&mag.to_string(), neg, ty)
}

/// Exact SMT-LIB2 IEEE `minimum` (docs/14: NaN propagates; `-0.0 < +0.0`).
pub fn smt_fmin(a: &str, b: &str, eb: u32, sb: u32) -> String {
    format!(
        "(ite (or (fp.isNaN {a}) (fp.isNaN {b})) (_ NaN {eb} {sb}) \
         (ite (fp.lt {a} {b}) {a} (ite (fp.lt {b} {a}) {b} (ite (fp.isNegative {a}) {a} {b}))))"
    )
}

/// Exact SMT-LIB2 IEEE `maximum` (docs/14: NaN propagates; `+0.0 > -0.0`).
pub fn smt_fmax(a: &str, b: &str, eb: u32, sb: u32) -> String {
    format!(
        "(ite (or (fp.isNaN {a}) (fp.isNaN {b})) (_ NaN {eb} {sb}) \
         (ite (fp.gt {a} {b}) {a} (ite (fp.gt {b} {a}) {b} (ite (fp.isPositive {a}) {a} {b}))))"
    )
}

/// Exact SMT-LIB2 `FCOPYSIGN(x, y)` (docs/14: NaN when `x` is NaN).
pub fn smt_fcopysign(x: &str, y: &str, eb: u32, sb: u32) -> String {
    format!(
        "(ite (fp.isNaN {x}) (_ NaN {eb} {sb}) \
         (ite (fp.isNegative {y}) (fp.neg (fp.abs {x})) (fp.abs {x})))"
    )
}

/// Exact `CAST` range condition for truncating float `x` (`src_ty`) into integer `dst_ty`.
pub fn smt_float_fits_int(x: &str, src_ty: &Type, dst_ty: &Type) -> String {
    let (_, sb) = fp_format(src_ty).expect("float source type");
    let finite = format!("(and (not (fp.isNaN {x})) (not (fp.isInfinite {x})))");
    match dst_ty {
        Type::UInt(n) => {
            let neg_one = fp_lit(src_ty, int_to_fp_bits(src_ty, 1, true).expect("-1 fits"));
            let lo = format!("(fp.gt {x} {neg_one})");
            if *src_ty == Type::Float(16) && *n >= 16 {
                // Max finite f16 is 65504 < 2^16, so every finite f16 fits below 2^N.
                format!("(and {finite} {lo})")
            } else {
                let bound = 1u128 << *n;
                let hi_lit = fp_lit(
                    src_ty,
                    int_to_fp_bits(src_ty, bound, false).expect("2^N fits in float"),
                );
                format!("(and {finite} {lo} (fp.lt {x} {hi_lit}))")
            }
        }
        Type::SInt(n) => {
            let n_u32 = u32::from(*n);
            if *src_ty == Type::Float(16) && n_u32 >= 32 {
                // Max finite magnitude of f16 is 65504 < 2^31.
                return finite;
            }
            let half = 1u128 << (n_u32 - 1);
            let hi_lit = fp_lit(
                src_ty,
                int_to_fp_bits(src_ty, half, false).expect("2^(N-1) fits in float"),
            );
            let hi = format!("(fp.lt {x} {hi_lit})");
            let lo = if n_u32 - 1 < sb {
                let lo_lit = fp_lit(
                    src_ty,
                    int_to_fp_bits(src_ty, half + 1, true).expect("-(2^(N-1)+1) exact"),
                );
                format!("(fp.gt {x} {lo_lit})")
            } else {
                let lo_lit = fp_lit(
                    src_ty,
                    int_to_fp_bits(src_ty, half, true).expect("-2^(N-1) exact"),
                );
                format!("(fp.geq {x} {lo_lit})")
            };
            format!("(and {finite} {lo} {hi})")
        }
        _ => unreachable!("smt_float_fits_int expects integer target type"),
    }
}

/// Flattens a VG type into deterministic `(suffix, SmtSort)` leaves (Bitwuzla-compatible, no datatypes).
pub fn flatten_type(ty: &Type) -> Vec<(String, SmtSort)> {
    match ty {
        Type::Bool => vec![(String::new(), SmtSort::Bool)],
        Type::UInt(w) | Type::SInt(w) => vec![(String::new(), SmtSort::BitVec(u32::from(*w)))],
        Type::Float(_) | Type::BFloat16 => {
            let (eb, sb) = fp_format(ty).expect("float format");
            vec![(String::new(), SmtSort::Fp(eb, sb))]
        }
        Type::Array(elem, _) | Type::Tensor(elem, _) => flatten_type(elem)
            .into_iter()
            .enumerate()
            .map(|(i, (_, s))| {
                (
                    format!("_arr{i}"),
                    SmtSort::Array(Box::new(SmtSort::BitVec(64)), Box::new(s)),
                )
            })
            .collect(),
        Type::Slice(elem) => {
            let mut out = vec![("_len".to_string(), SmtSort::BitVec(64))];
            for (i, (_, s)) in flatten_type(elem).into_iter().enumerate() {
                out.push((
                    format!("_arr{i}"),
                    SmtSort::Array(Box::new(SmtSort::BitVec(64)), Box::new(s)),
                ));
            }
            out
        }
        Type::Tuple(ms) => {
            let mut out = Vec::new();
            for (idx, m) in ms.iter().enumerate() {
                for (sfx, s) in flatten_type(m) {
                    out.push((format!("_{idx}{sfx}"), s));
                }
            }
            out
        }
        Type::Result(ok, err) => {
            let mut out = vec![("_is_ok".to_string(), SmtSort::Bool)];
            for (sfx, s) in flatten_type(ok) {
                out.push((format!("_ok{sfx}"), s));
            }
            for (sfx, s) in flatten_type(err) {
                out.push((format!("_err{sfx}"), s));
            }
            out
        }
        Type::Handle(_) => vec![(String::new(), SmtSort::BitVec(32))],
        Type::Eff | Type::Arena(_) | Type::Int => Vec::new(),
    }
}

/// Symbolic representation of any VG value in SMT-LIB2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymVal {
    Bool(String),
    Int {
        expr: String,
        ty: Type,
    },
    Float {
        expr: String,
        ty: Type,
    },
    Array {
        coll_id: u32,
        len: u64,
        elem_ty: Type,
        arrays: Vec<String>,
    },
    Tensor {
        coll_id: u32,
        dims: Vec<u32>,
        elem_ty: Type,
        arrays: Vec<String>,
    },
    Slice {
        coll_id: u32,
        len: String,
        elem_ty: Type,
        arrays: Vec<String>,
    },
    Tuple {
        elems: Vec<(Type, SymVal)>,
    },
    Result {
        is_ok: String,
        ok_ty: Type,
        ok: Box<SymVal>,
        err_ty: Type,
        err: Box<SymVal>,
    },
    Handle(String),
    Eff,
}

impl SymVal {
    pub fn coll_id(&self) -> Option<u32> {
        match self {
            SymVal::Array { coll_id, .. }
            | SymVal::Tensor { coll_id, .. }
            | SymVal::Slice { coll_id, .. } => Some(*coll_id),
            _ => None,
        }
    }

    pub fn arrays(&self) -> Option<&[String]> {
        match self {
            SymVal::Array { arrays, .. }
            | SymVal::Tensor { arrays, .. }
            | SymVal::Slice { arrays, .. } => Some(arrays),
            _ => None,
        }
    }
}

/// Declares fresh SMT constants for `ty` under `base`, returning its `SymVal`.
pub fn declare_symval(
    base: &str,
    ty: &Type,
    decls: &mut BTreeMap<String, String>,
    next_coll_id: &mut u32,
) -> SymVal {
    let flat = flatten_type(ty);
    let mut atom_names = Vec::with_capacity(flat.len());
    for (sfx, sort) in flat {
        let name = format!("{base}{sfx}");
        decls.insert(name.clone(), sort.to_string());
        atom_names.push(name);
    }
    symval_from_atoms(ty, &atom_names, next_coll_id)
}

/// Reconstructs a `SymVal` from flattened SMT expressions matching `flatten_type(ty)`.
pub fn symval_from_atoms(ty: &Type, atoms: &[String], next_coll_id: &mut u32) -> SymVal {
    let mut pos = 0usize;
    let val = take_from_atoms(ty, atoms, &mut pos, next_coll_id);
    debug_assert_eq!(pos, atoms.len());
    val
}

fn take_from_atoms(ty: &Type, atoms: &[String], pos: &mut usize, next_coll_id: &mut u32) -> SymVal {
    match ty {
        Type::Bool => {
            let e = atoms[*pos].clone();
            *pos += 1;
            SymVal::Bool(e)
        }
        Type::UInt(_) | Type::SInt(_) => {
            let e = atoms[*pos].clone();
            *pos += 1;
            SymVal::Int {
                expr: e,
                ty: ty.clone(),
            }
        }
        Type::Float(_) | Type::BFloat16 => {
            let e = atoms[*pos].clone();
            *pos += 1;
            SymVal::Float {
                expr: e,
                ty: ty.clone(),
            }
        }
        Type::Array(elem, len) => {
            let k = flatten_type(elem).len();
            let arrays = atoms[*pos..*pos + k].to_vec();
            *pos += k;
            let coll_id = *next_coll_id;
            *next_coll_id += 1;
            SymVal::Array {
                coll_id,
                len: u64::from(*len),
                elem_ty: (**elem).clone(),
                arrays,
            }
        }
        Type::Tensor(elem, dims) => {
            let k = flatten_type(elem).len();
            let arrays = atoms[*pos..*pos + k].to_vec();
            *pos += k;
            let coll_id = *next_coll_id;
            *next_coll_id += 1;
            SymVal::Tensor {
                coll_id,
                dims: dims.clone(),
                elem_ty: (**elem).clone(),
                arrays,
            }
        }
        Type::Slice(elem) => {
            let len = atoms[*pos].clone();
            *pos += 1;
            let k = flatten_type(elem).len();
            let arrays = atoms[*pos..*pos + k].to_vec();
            *pos += k;
            let coll_id = *next_coll_id;
            *next_coll_id += 1;
            SymVal::Slice {
                coll_id,
                len,
                elem_ty: (**elem).clone(),
                arrays,
            }
        }
        Type::Tuple(ms) => {
            let elems = ms
                .iter()
                .map(|m| (m.clone(), take_from_atoms(m, atoms, pos, next_coll_id)))
                .collect();
            SymVal::Tuple { elems }
        }
        Type::Result(ok_ty, err_ty) => {
            let is_ok = atoms[*pos].clone();
            *pos += 1;
            let ok = take_from_atoms(ok_ty, atoms, pos, next_coll_id);
            let err = take_from_atoms(err_ty, atoms, pos, next_coll_id);
            SymVal::Result {
                is_ok,
                ok_ty: (**ok_ty).clone(),
                ok: Box::new(ok),
                err_ty: (**err_ty).clone(),
                err: Box::new(err),
            }
        }
        Type::Handle(_) => {
            let e = atoms[*pos].clone();
            *pos += 1;
            SymVal::Handle(e)
        }
        Type::Eff | Type::Arena(_) | Type::Int => SymVal::Eff,
    }
}

/// Extracts flattened SMT expressions from `v` in `flatten_type` order.
pub fn symval_atoms(v: &SymVal) -> Vec<String> {
    let mut out = Vec::new();
    push_atoms(v, &mut out);
    out
}

fn push_atoms(v: &SymVal, out: &mut Vec<String>) {
    match v {
        SymVal::Bool(e)
        | SymVal::Int { expr: e, .. }
        | SymVal::Float { expr: e, .. }
        | SymVal::Handle(e) => out.push(e.clone()),
        SymVal::Array { arrays, .. } | SymVal::Tensor { arrays, .. } => {
            out.extend(arrays.iter().cloned());
        }
        SymVal::Slice { len, arrays, .. } => {
            out.push(len.clone());
            out.extend(arrays.iter().cloned());
        }
        SymVal::Tuple { elems } => {
            for (_, m) in elems {
                push_atoms(m, out);
            }
        }
        SymVal::Result { is_ok, ok, err, .. } => {
            out.push(is_ok.clone());
            push_atoms(ok, out);
            push_atoms(err, out);
        }
        SymVal::Eff => {}
    }
}

/// Constructs `(ite cond a b)` component-wise on two `SymVal`s of identical type `ty`.
pub fn symval_ite(cond: &str, ty: &Type, a: &SymVal, b: &SymVal, next_coll_id: &mut u32) -> SymVal {
    let aa = symval_atoms(a);
    let ba = symval_atoms(b);
    let combined: Vec<String> = aa
        .into_iter()
        .zip(ba)
        .map(|(x, y)| {
            if x == y {
                x
            } else {
                format!("(ite {cond} {x} {y})")
            }
        })
        .collect();
    symval_from_atoms(ty, &combined, next_coll_id)
}

/// Reads element at 64-bit bitvector index `idx_bv64` from an Array, Tensor, or Slice.
pub fn symval_select(coll: &SymVal, idx_bv64: &str, next_coll_id: &mut u32) -> SymVal {
    let (elem_ty, arrays) = match coll {
        SymVal::Array {
            elem_ty, arrays, ..
        }
        | SymVal::Tensor {
            elem_ty, arrays, ..
        }
        | SymVal::Slice {
            elem_ty, arrays, ..
        } => (elem_ty, arrays),
        _ => panic!("symval_select called on non-collection: {coll:?}"),
    };
    let elem_atoms: Vec<String> = arrays
        .iter()
        .map(|a| format!("(select {a} {idx_bv64})"))
        .collect();
    symval_from_atoms(elem_ty, &elem_atoms, next_coll_id)
}

/// Writes element `val` at 64-bit bitvector index `idx_bv64` into an Array or Slice.
pub fn symval_store(coll: &SymVal, idx_bv64: &str, val: &SymVal, next_coll_id: &mut u32) -> SymVal {
    let val_atoms = symval_atoms(val);
    let coll_id = *next_coll_id;
    *next_coll_id += 1;
    match coll {
        SymVal::Array {
            len,
            elem_ty,
            arrays,
            ..
        } => SymVal::Array {
            coll_id,
            len: *len,
            elem_ty: elem_ty.clone(),
            arrays: arrays
                .iter()
                .zip(val_atoms)
                .map(|(a, v)| format!("(store {a} {idx_bv64} {v})"))
                .collect(),
        },
        SymVal::Slice {
            len,
            elem_ty,
            arrays,
            ..
        } => SymVal::Slice {
            coll_id,
            len: len.clone(),
            elem_ty: elem_ty.clone(),
            arrays: arrays
                .iter()
                .zip(val_atoms)
                .map(|(a, v)| format!("(store {a} {idx_bv64} {v})"))
                .collect(),
        },
        _ => panic!("symval_store called on unsupported collection: {coll:?}"),
    }
}

/// Builds SMT-LIB2 `same(a, b)` over any two `SymVal`s of `ty` (only finite/small fixed collections
/// are unrolled here; larger collections use `formula.rs`).
pub fn scalar_same_smt(ty: &Type, a: &SymVal, b: &SymVal) -> Option<String> {
    match (ty, a, b) {
        (Type::Bool, SymVal::Bool(x), SymVal::Bool(y)) => Some(format!("(= {x} {y})")),
        (
            Type::UInt(_) | Type::SInt(_),
            SymVal::Int { expr: x, .. },
            SymVal::Int { expr: y, .. },
        ) => Some(format!("(= {x} {y})")),
        (
            Type::Float(_) | Type::BFloat16,
            SymVal::Float { expr: x, .. },
            SymVal::Float { expr: y, .. },
        ) => Some(format!(
            "(or (and (fp.isNaN {x}) (fp.isNaN {y})) \
                 (and (fp.eq {x} {y}) (= (fp.isNegative {x}) (fp.isNegative {y}))))"
        )),
        (Type::Handle(_), SymVal::Handle(x), SymVal::Handle(y)) => Some(format!("(= {x} {y})")),
        (Type::Eff, SymVal::Eff, SymVal::Eff) => Some("true".to_string()),
        (Type::Tuple(ts), SymVal::Tuple { elems: ea }, SymVal::Tuple { elems: eb }) => {
            let mut parts = Vec::new();
            for ((t, (_, va)), (_, vb)) in ts.iter().zip(ea).zip(eb) {
                parts.push(scalar_same_smt(t, va, vb)?);
            }
            if parts.is_empty() {
                Some("true".to_string())
            } else if parts.len() == 1 {
                Some(parts.remove(0))
            } else {
                Some(format!("(and {})", parts.join(" ")))
            }
        }
        (
            Type::Result(ok_t, err_t),
            SymVal::Result {
                is_ok: oa,
                ok: va_ok,
                err: va_err,
                ..
            },
            SymVal::Result {
                is_ok: ob,
                ok: vb_ok,
                err: vb_err,
                ..
            },
        ) => {
            let ok_eq = scalar_same_smt(ok_t, va_ok, vb_ok)?;
            let err_eq = scalar_same_smt(err_t, va_err, vb_err)?;
            Some(format!("(and (= {oa} {ob}) (ite {oa} {ok_eq} {err_eq}))"))
        }
        _ => None,
    }
}

/// Decodes a scalar/Bool constant's raw little-endian bytes into an SMT-LIB2 literal.
pub fn scalar_const_smt(ty: &Type, bytes: &[u8]) -> String {
    match ty {
        Type::Bool => (bytes[0] != 0).to_string(),
        Type::UInt(w) | Type::SInt(w) => {
            let mut buf = [0u8; 16];
            buf[..bytes.len()].copy_from_slice(bytes);
            bv_lit_u128(u128::from_le_bytes(buf), u32::from(*w))
        }
        Type::Float(_) | Type::BFloat16 => {
            let mut buf = [0u8; 8];
            buf[..bytes.len()].copy_from_slice(bytes);
            fp_lit(ty, u64::from_le_bytes(buf))
        }
        _ => panic!("scalar_const_smt called with non-scalar type {ty}"),
    }
}

/// Materializes an `Array` or `Tensor` constant from `Const`, declaring backing SMT arrays in
/// `decls` + element axioms in `assertions`.
pub fn const_to_symval(
    c: &Const,
    name_hint: &str,
    decls: &mut BTreeMap<String, String>,
    assertions: &mut Vec<String>,
    next_coll_id: &mut u32,
) -> SymVal {
    match &c.ty {
        Type::Bool | Type::UInt(_) | Type::SInt(_) => match &c.ty {
            Type::Bool => SymVal::Bool(scalar_const_smt(&c.ty, &c.bytes)),
            _ => SymVal::Int {
                expr: scalar_const_smt(&c.ty, &c.bytes),
                ty: c.ty.clone(),
            },
        },
        Type::Float(_) | Type::BFloat16 => SymVal::Float {
            expr: scalar_const_smt(&c.ty, &c.bytes),
            ty: c.ty.clone(),
        },
        Type::Array(elem, len) => {
            let sym = declare_symval(name_hint, &c.ty, decls, next_coll_id);
            emit_const_array_elements(
                elem,
                usize::try_from(*len).expect("array len fits usize"),
                &c.bytes,
                &sym,
                name_hint,
                decls,
                assertions,
                next_coll_id,
            );
            sym
        }
        Type::Tensor(elem, dims) => {
            let total: usize = dims
                .iter()
                .map(|&d| usize::try_from(d).expect("dim fits usize"))
                .product();
            let sym = declare_symval(name_hint, &c.ty, decls, next_coll_id);
            emit_const_array_elements(
                elem,
                total,
                &c.bytes,
                &sym,
                name_hint,
                decls,
                assertions,
                next_coll_id,
            );
            sym
        }
        _ => panic!("unsupported constant type in VG-IR: {}", c.ty),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_const_array_elements(
    elem: &Type,
    count: usize,
    bytes: &[u8],
    sym: &SymVal,
    hint: &str,
    decls: &mut BTreeMap<String, String>,
    assertions: &mut Vec<String>,
    next_coll_id: &mut u32,
) {
    let elem_size = elem
        .byte_size()
        .expect("array/tensor constants hold byte-sized elements") as usize;
    let arrays = sym.arrays().expect("collection has arrays");
    for idx in 0..count {
        let slice = &bytes[idx * elem_size..(idx + 1) * elem_size];
        let elem_const = Const {
            ty: elem.clone(),
            bytes: slice.to_vec(),
        };
        let elem_hint = format!("{hint}_e{idx}");
        let elem_sym = const_to_symval(&elem_const, &elem_hint, decls, assertions, next_coll_id);
        let elem_atoms = symval_atoms(&elem_sym);
        let idx_lit = bv_lit_u128(idx as u128, 64);
        for (arr, val) in arrays.iter().zip(elem_atoms) {
            assertions.push(format!("(= (select {arr} {idx_lit}) {val})"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_matches_declared_atoms() {
        let complex = Type::Tuple(vec![
            Type::Slice(Box::new(Type::Tuple(vec![Type::Float(32), Type::UInt(8)]))),
            Type::Result(
                Box::new(Type::Array(Box::new(Type::SInt(16)), 3)),
                Box::new(Type::Bool),
            ),
            Type::BFloat16,
        ]);
        let mut decls = BTreeMap::new();
        let mut cid = 0;
        let sym = declare_symval("a0", &complex, &mut decls, &mut cid);
        assert_eq!(symval_atoms(&sym).len(), flatten_type(&complex).len());
        assert_eq!(decls.len(), flatten_type(&complex).len());
    }

    #[test]
    fn float_fit_bounds_are_exact() {
        let cond = smt_float_fits_int("x", &Type::Float(32), &Type::SInt(32));
        assert!(cond.contains("fp.lt x"));
        assert!(cond.contains("fp.geq x"));
        let cond_i8 = smt_float_fits_int("x", &Type::Float(32), &Type::SInt(8));
        assert!(cond_i8.contains("fp.gt x"));
        let cond_u32 = smt_float_fits_int("x", &Type::Float(64), &Type::UInt(32));
        assert!(cond_u32.contains("fp.gt x"));
        assert!(cond_u32.contains("fp.lt x"));
    }
}
