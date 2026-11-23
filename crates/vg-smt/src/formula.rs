//! Formula encoding, exact bitvector integer widths (docs/05), well-definedness (ADR 0010),
//! `fold:` / `spec:` axioms (ADR 0008), `as_bytes`, and pure kernel inlining.

use std::collections::{BTreeMap, BTreeSet};

use vg_core::ir::{Bin, Builtin, Hash, Node, Op, Operand, Resource, Term};
use vg_core::lower::int_value;
use vg_core::opcode;
use vg_core::types::Type;

use crate::solver::SmtQuery;
use crate::sort::{
    SymVal, bv_lit_i128, bv_lit_u128, canonical_nan_bv, const_to_symval, declare_symval,
    flatten_type, fp_bit_width, fp_format, scalar_same_smt, smt_fcopysign, smt_float_fits_int,
    smt_fmax, smt_fmin, symval_atoms, symval_from_atoms, symval_ite, symval_select, symval_store,
};

/// Symbolic integer/arena/heap resource expressions used when encoding `COST` clauses.
#[derive(Clone, Debug)]
pub struct CostResources {
    pub ops: FVal,
    pub arena_bytes: FVal,
    pub heap_bytes: FVal,
}

/// Typed value inside a contract formula.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FVal {
    Bool(String),
    Int {
        expr: String,
        width: u32,
        /// Exact typed BV backing if this came unmodified from a VG integer value of `Type`.
        orig: Option<(Type, String)>,
    },
    Float {
        expr: String,
        ty: Type,
    },
    Val {
        val: SymVal,
        ty: Type,
    },
}

impl FVal {
    pub fn as_bool(&self) -> Option<&str> {
        match self {
            FVal::Bool(e) => Some(e),
            _ => None,
        }
    }

    pub fn into_int(self) -> Option<(String, u32)> {
        match self {
            FVal::Int { expr, width, .. } => Some((expr, width)),
            _ => None,
        }
    }
}

/// Bit width for an integer literal `v` (docs/05: `bits(|v|) + 1`, so `-v` never hits MIN).
pub fn int_literal_width(v: i128) -> u32 {
    let abs = v.unsigned_abs();
    let mag_bits = if abs == 0 {
        1
    } else {
        128 - abs.leading_zeros()
    };
    (mag_bits + 1).max(2)
}

pub fn sext(expr: &str, from_w: u32, to_w: u32) -> String {
    debug_assert!(to_w >= from_w);
    if to_w == from_w {
        expr.to_string()
    } else {
        format!("((_ sign_extend {}) {expr})", to_w - from_w)
    }
}

pub fn zext(expr: &str, from_w: u32, to_w: u32) -> String {
    debug_assert!(to_w >= from_w);
    if to_w == from_w {
        expr.to_string()
    } else {
        format!("((_ zero_extend {}) {expr})", to_w - from_w)
    }
}

pub fn extract_lo(expr: &str, from_w: u32, to_w: u32) -> String {
    debug_assert!(to_w <= from_w);
    if to_w == from_w {
        expr.to_string()
    } else {
        format!("((_ extract {} 0) {expr})", to_w - 1)
    }
}

/// Converts a VG `SymVal` of type `ty` to a formula `FVal` (widening fixed integers to `N + 1` signed bits).
pub fn symval_to_fval(val: &SymVal, ty: &Type) -> FVal {
    match (ty, val) {
        (Type::Bool, SymVal::Bool(e)) => FVal::Bool(e.clone()),
        (Type::UInt(w), SymVal::Int { expr, .. }) => {
            let w32 = u32::from(*w);
            FVal::Int {
                expr: zext(expr, w32, w32 + 1),
                width: w32 + 1,
                orig: Some((ty.clone(), expr.clone())),
            }
        }
        (Type::SInt(w), SymVal::Int { expr, .. }) => {
            let w32 = u32::from(*w);
            FVal::Int {
                expr: sext(expr, w32, w32 + 1),
                width: w32 + 1,
                orig: Some((ty.clone(), expr.clone())),
            }
        }
        (Type::Float(_) | Type::BFloat16, SymVal::Float { expr, .. }) => FVal::Float {
            expr: expr.clone(),
            ty: ty.clone(),
        },
        _ => FVal::Val {
            val: val.clone(),
            ty: ty.clone(),
        },
    }
}

/// Converts a formula `FVal` back into a VG `SymVal` of `target_ty` (truncating integer width cleanly).
pub fn fval_to_symval(f: &FVal, target_ty: &Type) -> Option<SymVal> {
    match (f, target_ty) {
        (FVal::Bool(e), Type::Bool) => Some(SymVal::Bool(e.clone())),
        (FVal::Int { expr, width, orig }, Type::UInt(w) | Type::SInt(w)) => {
            if let Some((orig_ty, orig_expr)) = orig
                && orig_ty == target_ty
            {
                return Some(SymVal::Int {
                    expr: orig_expr.clone(),
                    ty: target_ty.clone(),
                });
            }
            let w32 = u32::from(*w);
            let bv = if *width >= w32 {
                extract_lo(expr, *width, w32)
            } else {
                sext(expr, *width, w32)
            };
            Some(SymVal::Int {
                expr: bv,
                ty: target_ty.clone(),
            })
        }
        (FVal::Float { expr, ty }, _) if ty == target_ty => Some(SymVal::Float {
            expr: expr.clone(),
            ty: ty.clone(),
        }),
        (FVal::Val { val, .. }, _) => Some(val.clone()),
        _ => None,
    }
}

/// An observed index access on a collection (either 1D flat `bv64`, or multi-D `dims` + `flat`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexAccess {
    pub dims: Vec<String>,
    pub flat: String,
}

#[derive(Clone, Debug)]
pub enum CollLink {
    /// Two collections share indices identically (e.g. `SIMD_MAP` output <-> input, `UPDATE` dst <-> src).
    Identity { dst: u32, src: u32 },
    /// `SLICE(src, lo, hi)`: index `idx` on `dst` corresponds to `lo + idx` on `src`.
    Offset { dst: u32, src: u32, offset: String },
    /// `CONCAT(a, b)`: index `idx` on `dst` corresponds to `idx` on `a` or `idx - len_a` on `b`.
    Concat {
        dst: u32,
        a: u32,
        b: u32,
        len_a: String,
    },
    /// `SELECT(c, then_v, else_v)`: propagate index accesses on `dst` to both branches.
    Ite {
        dst: u32,
        then_id: u32,
        else_id: u32,
    },
    /// `AS_BYTES(src)` where each `src` element is `elem_bytes > 1`: byte index `i` maps to `i / K`.
    AsBytes { dst: u32, src: u32, elem_bytes: u64 },
}

#[derive(Clone, Debug)]
pub enum ElemSchema {
    SimdMap {
        dst_coll: u32,
        dst_ty: Type,
        dst_arrays: Vec<String>,
        src_val: SymVal,
        src_ty: Type,
        broadcasts: Vec<(Type, SymVal)>,
        kernel: Node,
        kernel_hash: Hash,
    },
    Generate {
        dst_coll: u32,
        dst_ty: Type,
        dst_arrays: Vec<String>,
        count_bv64: String,
        broadcasts: Vec<(Type, SymVal)>,
        kernel: Node,
        kernel_hash: Hash,
    },
    Slice {
        dst_coll: u32,
        dst_arrays: Vec<String>,
        src_arrays: Vec<String>,
        slice_len: String,
        lo: String,
    },
    Concat {
        dst_coll: u32,
        dst_arrays: Vec<String>,
        a_arrays: Vec<String>,
        b_arrays: Vec<String>,
        len_a: String,
        len_b: String,
    },
    AsBytes {
        dst_coll: u32,
        dst_arr: String,
        src_arr: String,
        src_elem_ty: Type,
        elem_bytes: u64,
        total_bytes_bv64: String,
    },
}

#[derive(Clone, Debug)]
pub struct ForallWatch {
    pub coll_id: u32,
    /// If non-empty and matches `r` bound variables directly (`[i0, i1, ...]`), instantiate directly.
    pub dim_patterns: Vec<IdxPattern>,
}

#[derive(Clone, Debug)]
pub enum IdxPattern {
    /// Bound variable `var_pos` (0 = outermost range of this forall).
    Var(usize),
    /// `var + offset` where `offset` is a constant `i128`
    VarPlus(usize, i128),
    /// `offset - var`
    ConstMinusVar(i128, usize),
}

#[derive(Clone, Debug)]
pub struct SavedEnv {
    pub node: Node,
    pub inputs: Vec<(Type, SymVal)>,
    pub results: Vec<(Type, SymVal)>,
    pub ports: BTreeMap<(u32, u8), (Type, SymVal)>,
    pub bound: Vec<FVal>,
    pub resources: Option<CostResources>,
}

#[derive(Clone, Debug)]
// Short-lived queue entries; boxing the large variant would only add indirection.
#[allow(clippy::large_enum_variant)]
pub enum PendingForall {
    Term {
        ranges: Vec<(Term, Term)>,
        body: Term,
        env: SavedEnv,
        watches: Vec<ForallWatch>,
    },
    CollEq {
        len_bv64: String,
        left: SymVal,
        right: SymVal,
        elem_ty: Type,
        is_same: bool,
        env_node: Node,
    },
}

/// Accumulated SMT declarations, assertions, pure definitions, and instantiation queues.
#[derive(Clone, Debug, Default)]
pub struct QueryBuilder {
    pub decls: BTreeMap<String, String>,
    pub fun_decls: BTreeMap<String, (Vec<String>, String)>,
    pub always_keep_decls: BTreeSet<String>,
    /// `(defined_symbols, defining_equality_assertion)` for cone-of-influence pruning.
    pub pure_defs: Vec<(BTreeSet<String>, String)>,
    /// `(anchor_symbol, axiom)` kept only if `anchor_symbol` survives pruning.
    pub dep_axioms: Vec<(String, String)>,
    /// Non-droppable assertions (PRE, LEARNABLE, goals, contract facts, instantiated schemas).
    pub assertions: Vec<String>,
    pub next_fresh: u32,
    pub next_coll_id: u32,
    pub coll_intern: BTreeMap<(Option<String>, Vec<String>), u32>,
    pub index_pool: BTreeMap<u32, BTreeSet<IndexAccess>>,
    pub coll_links: Vec<CollLink>,
    pub pending_foralls: Vec<PendingForall>,
    pub pending_schemas: Vec<ElemSchema>,
    pub fold_apps: BTreeSet<String>,
    pub spec_apps: BTreeSet<String>,
    pub fp_to_bv_apps: BTreeSet<String>,
    pub instantiated_foralls: BTreeSet<(usize, Vec<String>)>,
    pub instantiated_schemas: BTreeSet<(usize, Vec<String>)>,
    /// When set, the query cannot be proven (e.g., missing callee/spec or unprovable resource).
    pub unresolved_error: Option<String>,
    /// `(tag, domain)` of every partial op evaluated in DomainMode::Strict since the last clear.
    pub domains: Vec<(String, String)>,
}

impl QueryBuilder {
    pub fn fresh_sym(&mut self, prefix: &str, sort: &str) -> String {
        let id = self.next_fresh;
        self.next_fresh += 1;
        let name = format!("f_{prefix}_{id}");
        self.decls.insert(name.clone(), sort.to_string());
        name
    }

    pub fn fresh_symval(&mut self, prefix: &str, ty: &Type) -> SymVal {
        let id = self.next_fresh;
        self.next_fresh += 1;
        let base = format!("f_{prefix}_{id}");
        let sv = declare_symval(&base, ty, &mut self.decls, &mut self.next_coll_id);
        self.register_coll_id(&sv);
        sv
    }

    pub fn intern_coll(&mut self, len: Option<String>, arrays: &[String]) -> u32 {
        let key = (len, arrays.to_vec());
        if let Some(&id) = self.coll_intern.get(&key) {
            return id;
        }
        let id = self.next_coll_id;
        self.next_coll_id += 1;
        self.coll_intern.insert(key, id);
        id
    }

    pub fn register_coll_id(&mut self, sv: &SymVal) {
        match sv {
            SymVal::Array {
                coll_id, arrays, ..
            }
            | SymVal::Tensor {
                coll_id, arrays, ..
            } => {
                self.coll_intern
                    .entry((None, arrays.clone()))
                    .or_insert(*coll_id);
            }
            SymVal::Slice {
                coll_id,
                len,
                arrays,
                ..
            } => {
                self.coll_intern
                    .entry((Some(len.clone()), arrays.clone()))
                    .or_insert(*coll_id);
            }
            SymVal::Tuple { elems } => {
                for (_, m) in elems {
                    self.register_coll_id(m);
                }
            }
            SymVal::Result { ok, err, .. } => {
                self.register_coll_id(ok);
                self.register_coll_id(err);
            }
            _ => {}
        }
    }

    pub fn record_index(&mut self, sv: &SymVal, dims: Vec<String>, flat: String) {
        if let Some(cid) = sv.coll_id() {
            self.index_pool
                .entry(cid)
                .or_default()
                .insert(IndexAccess { dims, flat });
        }
    }

    pub fn declare_fun_once(&mut self, name: &str, args: Vec<String>, ret: String) {
        self.fun_decls
            .entry(name.to_string())
            .or_insert((args, ret));
    }

    /// Ensures a pure defining equality `(= sym rhs)` is tracked for cone-of-influence pruning.
    pub fn add_pure_def(&mut self, syms: BTreeSet<String>, eq: String) {
        self.pure_defs.push((syms, eq));
    }

    /// Declares a pure op's outputs and emits defining equalities into `pure_defs`.
    pub fn bind_pure_symval(&mut self, syms: &[String], rhs_val: &SymVal) {
        let rhs_atoms = symval_atoms(rhs_val);
        debug_assert_eq!(syms.len(), rhs_atoms.len());
        if syms.is_empty() {
            return;
        }
        let mut defined = BTreeSet::new();
        let mut eqs = Vec::with_capacity(syms.len());
        for (s, r) in syms.iter().zip(rhs_atoms) {
            defined.insert(s.clone());
            eqs.push(format!("(= {s} {r})"));
        }
        let combined = if eqs.len() == 1 {
            eqs.remove(0)
        } else {
            format!("(and {})", eqs.join(" "))
        };
        self.add_pure_def(defined, combined);
    }

    /// Runs up to 4 rounds of `CollLink` propagation + `PendingForall` + `ElemSchema` instantiation.
    pub fn finalize_instantiations(&mut self, callee: &dyn Fn(&Hash) -> Option<Node>) {
        // Seed any small constant `PendingForall` ranges (total <= 8) and boundary points once.
        self.seed_pending_forall_bounds(callee);

        for _round in 0..4 {
            let mut changed = false;
            changed |= self.propagate_coll_links();
            changed |= self.instantiate_pending_foralls(callee);
            changed |= self.instantiate_elem_schemas(callee);
            if !changed {
                break;
            }
        }
    }

    fn seed_pending_forall_bounds(&mut self, callee: &dyn Fn(&Hash) -> Option<Node>) {
        let snapshot = self.pending_foralls.clone();
        for (idx, pf) in snapshot.iter().enumerate() {
            let PendingForall::Term {
                ranges, body, env, ..
            } = pf
            else {
                continue;
            };
            let env_ref = env.as_env(callee);
            // Evaluate constant bounds if top-level ranges don't depend on earlier binders.
            let mut const_bounds: Vec<(i128, i128)> = Vec::new();
            let mut all_const = true;
            for (lo, hi) in ranges {
                if let (Some(lv), Some(hv)) =
                    (eval_const_int(lo, &env.node), eval_const_int(hi, &env.node))
                {
                    const_bounds.push((lv, hv));
                } else {
                    all_const = false;
                    break;
                }
            }
            if all_const {
                let total: Option<u128> = const_bounds.iter().try_fold(1u128, |acc, &(l, h)| {
                    if h <= l {
                        Some(0)
                    } else {
                        acc.checked_mul((h - l) as u128)
                    }
                });
                if let Some(t) = total
                    && t <= 8
                {
                    let mut combos = vec![Vec::new()];
                    for &(l, h) in &const_bounds {
                        let mut next = Vec::new();
                        for c in combos {
                            for v in l..h {
                                let mut c2 = c.clone();
                                c2.push(v);
                                next.push(c2);
                            }
                        }
                        combos = next;
                    }
                    for vals in combos {
                        let strs: Vec<String> = vals.iter().map(|v| format!("const:{v}")).collect();
                        if !self.instantiated_foralls.insert((idx, strs)) {
                            continue;
                        }
                        let mut sub_env = env_ref.clone();
                        for v in vals {
                            let w = int_literal_width(v);
                            sub_env.bound.push(FVal::Int {
                                expr: bv_lit_i128(v, w),
                                width: w,
                                orig: None,
                            });
                        }
                        let inst = encode_assume(body, &mut sub_env, self);
                        self.assertions.push(inst);
                    }
                    continue;
                }
            }
            // Otherwise if the forall watches NO collections, instantiate at symbolic boundary `lo`.
            if let PendingForall::Term { watches, .. } = pf
                && watches.is_empty()
            {
                let mut sub_env = env_ref.clone();
                let mut key = Vec::new();
                let mut conds = Vec::new();
                for (lo, hi) in ranges {
                    let lv = eval_term(lo, &mut sub_env, self)
                        .into_int()
                        .expect("int bound");
                    let hv = eval_term(hi, &mut sub_env, self)
                        .into_int()
                        .expect("int bound");
                    let w = lv.1.max(hv.1);
                    let ls = sext(&lv.0, lv.1, w);
                    let hs = sext(&hv.0, hv.1, w);
                    conds.push(format!("(bvslt {ls} {hs})"));
                    key.push(ls);
                    sub_env.bound.push(FVal::Int {
                        expr: lv.0,
                        width: lv.1,
                        orig: None,
                    });
                }
                if self.instantiated_foralls.insert((idx, key)) {
                    let body_f = encode_assume(body, &mut sub_env, self);
                    let cond = if conds.len() == 1 {
                        conds.remove(0)
                    } else {
                        format!("(and {})", conds.join(" "))
                    };
                    self.assertions.push(format!("(=> {cond} {body_f})"));
                }
            }
        }
    }

    fn propagate_coll_links(&mut self) -> bool {
        let mut changed = false;
        let links = self.coll_links.clone();
        for link in links {
            match link {
                CollLink::Identity { dst, src } => {
                    let d_set = self.index_pool.get(&dst).cloned().unwrap_or_default();
                    let s_set = self.index_pool.get(&src).cloned().unwrap_or_default();
                    for item in &d_set {
                        if self.index_pool.entry(src).or_default().insert(item.clone()) {
                            changed = true;
                        }
                    }
                    for item in &s_set {
                        if self.index_pool.entry(dst).or_default().insert(item.clone()) {
                            changed = true;
                        }
                    }
                }
                CollLink::Offset { dst, src, offset } => {
                    let d_set = self.index_pool.get(&dst).cloned().unwrap_or_default();
                    for item in d_set {
                        let shifted = IndexAccess {
                            dims: Vec::new(),
                            flat: format!("(bvadd {offset} {})", item.flat),
                        };
                        if self.index_pool.entry(src).or_default().insert(shifted) {
                            changed = true;
                        }
                    }
                }
                CollLink::Concat { dst, a, b, len_a } => {
                    let d_set = self.index_pool.get(&dst).cloned().unwrap_or_default();
                    for item in d_set {
                        let ia = IndexAccess {
                            dims: Vec::new(),
                            flat: item.flat.clone(),
                        };
                        if self.index_pool.entry(a).or_default().insert(ia) {
                            changed = true;
                        }
                        let ib = IndexAccess {
                            dims: Vec::new(),
                            flat: format!("(bvsub {} {len_a})", item.flat),
                        };
                        if self.index_pool.entry(b).or_default().insert(ib) {
                            changed = true;
                        }
                    }
                }
                CollLink::Ite {
                    dst,
                    then_id,
                    else_id,
                } => {
                    let d_set = self.index_pool.get(&dst).cloned().unwrap_or_default();
                    for item in d_set {
                        if self
                            .index_pool
                            .entry(then_id)
                            .or_default()
                            .insert(item.clone())
                        {
                            changed = true;
                        }
                        if self.index_pool.entry(else_id).or_default().insert(item) {
                            changed = true;
                        }
                    }
                }
                CollLink::AsBytes {
                    dst,
                    src,
                    elem_bytes,
                } => {
                    let d_set = self.index_pool.get(&dst).cloned().unwrap_or_default();
                    let k_lit = bv_lit_u128(u128::from(elem_bytes), 64);
                    for item in d_set {
                        let e_idx = IndexAccess {
                            dims: Vec::new(),
                            flat: format!("(bvudiv {} {k_lit})", item.flat),
                        };
                        if self.index_pool.entry(src).or_default().insert(e_idx) {
                            changed = true;
                        }
                    }
                }
            }
        }
        changed
    }

    fn instantiate_pending_foralls(&mut self, callee: &dyn Fn(&Hash) -> Option<Node>) -> bool {
        let mut changed = false;
        let snapshot = self.pending_foralls.clone();
        for (idx, pf) in snapshot.iter().enumerate() {
            match pf {
                PendingForall::Term {
                    ranges,
                    body,
                    env,
                    watches,
                } => {
                    for w in watches {
                        let accesses = self.index_pool.get(&w.coll_id).cloned().unwrap_or_default();
                        for acc in accesses {
                            let Some(bound_vals) =
                                match_watch_to_range_vars(ranges.len(), &w.dim_patterns, &acc)
                            else {
                                continue;
                            };
                            let key: Vec<String> =
                                bound_vals.iter().map(|(e, _)| e.clone()).collect();
                            if !self.instantiated_foralls.insert((idx, key)) {
                                continue;
                            }
                            let mut sub_env = env.as_env(callee);
                            let mut range_conds = Vec::new();
                            for (m, (lo, hi)) in ranges.iter().enumerate() {
                                let (le, lw) = eval_term(lo, &mut sub_env, self)
                                    .into_int()
                                    .expect("int range lo");
                                let (he, hw) = eval_term(hi, &mut sub_env, self)
                                    .into_int()
                                    .expect("int range hi");
                                let (ve, vw) = &bound_vals[m];
                                let cw = lw.max(hw).max(*vw);
                                let ls = sext(&le, lw, cw);
                                let hs = sext(&he, hw, cw);
                                let vs = sext(ve, *vw, cw);
                                range_conds
                                    .push(format!("(and (bvsle {ls} {vs}) (bvslt {vs} {hs}))"));
                                sub_env.bound.push(FVal::Int {
                                    expr: ve.clone(),
                                    width: *vw,
                                    orig: None,
                                });
                            }
                            let body_smt = encode_assume(body, &mut sub_env, self);
                            let guard = if range_conds.len() == 1 {
                                range_conds.remove(0)
                            } else {
                                format!("(and {})", range_conds.join(" "))
                            };
                            self.assertions.push(format!("(=> {guard} {body_smt})"));
                            changed = true;
                        }
                    }
                }
                PendingForall::CollEq {
                    len_bv64,
                    left,
                    right,
                    elem_ty,
                    is_same,
                    env_node,
                } => {
                    let mut idxs = BTreeSet::new();
                    if let Some(lcid) = left.coll_id()
                        && let Some(set) = self.index_pool.get(&lcid)
                    {
                        for a in set {
                            idxs.insert(a.flat.clone());
                        }
                    }
                    if let Some(rcid) = right.coll_id()
                        && let Some(set) = self.index_pool.get(&rcid)
                    {
                        for a in set {
                            idxs.insert(a.flat.clone());
                        }
                    }
                    for flat in idxs {
                        if !self.instantiated_foralls.insert((idx, vec![flat.clone()])) {
                            continue;
                        }
                        let l_elem = symval_select(left, &flat, &mut self.next_coll_id);
                        let r_elem = symval_select(right, &flat, &mut self.next_coll_id);
                        self.register_coll_id(&l_elem);
                        self.register_coll_id(&r_elem);
                        let mut dummy_env = FormulaEnv {
                            node: env_node,
                            callee,
                            inputs: Vec::new(),
                            results: Vec::new(),
                            ports: BTreeMap::new(),
                            bound: Vec::new(),
                            resources: None,
                        };
                        let elem_eq = encode_val_eq_assume(
                            elem_ty,
                            &l_elem,
                            &r_elem,
                            *is_same,
                            &mut dummy_env,
                            self,
                        );
                        self.assertions
                            .push(format!("(=> (bvult {flat} {len_bv64}) {elem_eq})"));
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    fn instantiate_elem_schemas(&mut self, callee: &dyn Fn(&Hash) -> Option<Node>) -> bool {
        let mut changed = false;
        let snapshot = self.pending_schemas.clone();
        for (idx, schema) in snapshot.iter().enumerate() {
            match schema {
                ElemSchema::SimdMap {
                    dst_coll,
                    dst_ty,
                    dst_arrays,
                    src_val,
                    src_ty,
                    broadcasts,
                    kernel,
                    kernel_hash,
                } => {
                    let accesses = self.index_pool.get(dst_coll).cloned().unwrap_or_default();
                    let out_elem_ty = dst_ty.elem().expect("collection elem").clone();
                    let src_elem_ty = src_ty.elem().expect("collection elem").clone();
                    for acc in accesses {
                        if !self
                            .instantiated_schemas
                            .insert((idx, vec![acc.flat.clone()]))
                        {
                            continue;
                        }
                        let Some(in_bounds) = coll_in_bounds(src_val, &acc.flat) else {
                            continue;
                        };
                        let src_elem = symval_select(src_val, &acc.flat, &mut self.next_coll_id);
                        let dst_atoms: Vec<String> = dst_arrays
                            .iter()
                            .map(|a| format!("(select {a} {})", acc.flat))
                            .collect();
                        let dst_elem =
                            symval_from_atoms(&out_elem_ty, &dst_atoms, &mut self.next_coll_id);
                        self.register_coll_id(&src_elem);
                        self.register_coll_id(&dst_elem);

                        let mut k_args = Vec::with_capacity(1 + broadcasts.len());
                        k_args.push((src_elem_ty.clone(), src_elem));
                        k_args.extend(broadcasts.iter().cloned());

                        let (k_pre_ok, k_post_ok) =
                            kernel_contract_facts(kernel, &k_args, &dst_elem, callee, self);
                        let inlined = inline_pure_node(
                            kernel,
                            *kernel_hash,
                            &k_args,
                            &format!("map{idx}"),
                            callee,
                            self,
                            0,
                        );
                        let mut conjs = Vec::new();
                        if !k_post_ok.is_empty() {
                            conjs.push(k_post_ok);
                        }
                        if let Some(res_vec) = inlined
                            && let Some(out0) = res_vec.first()
                            && let Some(eq) = try_scalar_or_val_same(
                                &out_elem_ty,
                                &dst_elem,
                                out0,
                                kernel,
                                callee,
                                self,
                            )
                        {
                            conjs.push(eq);
                        }
                        if !conjs.is_empty() {
                            let rhs = if conjs.len() == 1 {
                                conjs.remove(0)
                            } else {
                                format!("(and {})", conjs.join(" "))
                            };
                            let guard = if k_pre_ok == "true" {
                                in_bounds
                            } else {
                                format!("(and {in_bounds} {k_pre_ok})")
                            };
                            self.assertions.push(format!("(=> {guard} {rhs})"));
                        }
                        changed = true;
                    }
                }
                ElemSchema::Generate {
                    dst_coll,
                    dst_ty,
                    dst_arrays,
                    count_bv64,
                    broadcasts,
                    kernel,
                    kernel_hash,
                } => {
                    let accesses = self.index_pool.get(dst_coll).cloned().unwrap_or_default();
                    let out_elem_ty = dst_ty.elem().expect("collection elem").clone();
                    for acc in accesses {
                        if !self
                            .instantiated_schemas
                            .insert((idx, vec![acc.flat.clone()]))
                        {
                            continue;
                        }
                        let in_bounds = format!("(bvult {} {count_bv64})", acc.flat);
                        let dst_atoms: Vec<String> = dst_arrays
                            .iter()
                            .map(|a| format!("(select {a} {})", acc.flat))
                            .collect();
                        let dst_elem =
                            symval_from_atoms(&out_elem_ty, &dst_atoms, &mut self.next_coll_id);
                        self.register_coll_id(&dst_elem);

                        let idx_sym = SymVal::Int {
                            expr: acc.flat.clone(),
                            ty: Type::UInt(64),
                        };
                        let mut k_args = Vec::with_capacity(1 + broadcasts.len());
                        k_args.push((Type::UInt(64), idx_sym));
                        k_args.extend(broadcasts.iter().cloned());

                        let (k_pre_ok, k_post_ok) =
                            kernel_contract_facts(kernel, &k_args, &dst_elem, callee, self);
                        let inlined = inline_pure_node(
                            kernel,
                            *kernel_hash,
                            &k_args,
                            &format!("gen{idx}"),
                            callee,
                            self,
                            0,
                        );
                        let mut conjs = Vec::new();
                        if !k_post_ok.is_empty() {
                            conjs.push(k_post_ok);
                        }
                        if let Some(res_vec) = inlined
                            && let Some(out0) = res_vec.first()
                            && let Some(eq) = try_scalar_or_val_same(
                                &out_elem_ty,
                                &dst_elem,
                                out0,
                                kernel,
                                callee,
                                self,
                            )
                        {
                            conjs.push(eq);
                        }
                        if !conjs.is_empty() {
                            let rhs = if conjs.len() == 1 {
                                conjs.remove(0)
                            } else {
                                format!("(and {})", conjs.join(" "))
                            };
                            let guard = if k_pre_ok == "true" {
                                in_bounds
                            } else {
                                format!("(and {in_bounds} {k_pre_ok})")
                            };
                            self.assertions.push(format!("(=> {guard} {rhs})"));
                        }
                        changed = true;
                    }
                }
                ElemSchema::Slice {
                    dst_coll,
                    dst_arrays,
                    src_arrays,
                    slice_len,
                    lo,
                } => {
                    let accesses = self.index_pool.get(dst_coll).cloned().unwrap_or_default();
                    for acc in accesses {
                        if !self
                            .instantiated_schemas
                            .insert((idx, vec![acc.flat.clone()]))
                        {
                            continue;
                        }
                        let src_idx = format!("(bvadd {lo} {})", acc.flat);
                        let eqs: Vec<String> = dst_arrays
                            .iter()
                            .zip(src_arrays)
                            .map(|(da, sa)| {
                                format!("(= (select {da} {}) (select {sa} {src_idx}))", acc.flat)
                            })
                            .collect();
                        let rhs = if eqs.len() == 1 {
                            eqs.into_iter().next().expect("1")
                        } else {
                            format!("(and {})", eqs.join(" "))
                        };
                        self.assertions
                            .push(format!("(=> (bvult {} {slice_len}) {rhs})", acc.flat));
                        changed = true;
                    }
                }
                ElemSchema::Concat {
                    dst_coll,
                    dst_arrays,
                    a_arrays,
                    b_arrays,
                    len_a,
                    len_b,
                } => {
                    let accesses = self.index_pool.get(dst_coll).cloned().unwrap_or_default();
                    for acc in accesses {
                        if !self
                            .instantiated_schemas
                            .insert((idx, vec![acc.flat.clone()]))
                        {
                            continue;
                        }
                        let a_eqs: Vec<String> = dst_arrays
                            .iter()
                            .zip(a_arrays)
                            .map(|(da, aa)| {
                                format!("(= (select {da} {}) (select {aa} {}))", acc.flat, acc.flat)
                            })
                            .collect();
                        let b_idx = format!("(bvsub {} {len_a})", acc.flat);
                        let b_eqs: Vec<String> = dst_arrays
                            .iter()
                            .zip(b_arrays)
                            .map(|(da, ba)| {
                                format!("(= (select {da} {}) (select {ba} {b_idx}))", acc.flat)
                            })
                            .collect();
                        let a_part = if a_eqs.len() == 1 {
                            a_eqs.into_iter().next().expect("1")
                        } else {
                            format!("(and {})", a_eqs.join(" "))
                        };
                        let b_part = if b_eqs.len() == 1 {
                            b_eqs.into_iter().next().expect("1")
                        } else {
                            format!("(and {})", b_eqs.join(" "))
                        };
                        let total_len = format!("(bvadd {len_a} {len_b})");
                        self.assertions.push(format!(
                            "(=> (bvult {} {total_len}) (ite (bvult {} {len_a}) {a_part} {b_part}))",
                            acc.flat, acc.flat
                        ));
                        changed = true;
                    }
                }
                ElemSchema::AsBytes {
                    dst_coll,
                    dst_arr,
                    src_arr,
                    src_elem_ty,
                    elem_bytes,
                    total_bytes_bv64,
                } => {
                    let accesses = self.index_pool.get(dst_coll).cloned().unwrap_or_default();
                    let k_lit = bv_lit_u128(u128::from(*elem_bytes), 64);
                    for acc in accesses {
                        if !self
                            .instantiated_schemas
                            .insert((idx, vec![acc.flat.clone()]))
                        {
                            continue;
                        }
                        let elem_idx = format!("(bvudiv {} {k_lit})", acc.flat);
                        let byte_off = format!("(bvurem {} {k_lit})", acc.flat);
                        let raw_elem = format!("(select {src_arr} {elem_idx})");
                        let elem_bv = scalar_to_bv_expr(src_elem_ty, &raw_elem, self);
                        let byte_val = extract_le_byte(&elem_bv, *elem_bytes, &byte_off);
                        self.assertions.push(format!(
                            "(=> (bvult {} {total_bytes_bv64}) (= (select {dst_arr} {}) {byte_val}))",
                            acc.flat, acc.flat
                        ));
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Builds the final pruned `SmtQuery` by dropping unreferenced pure definitions and their symbols.
    pub fn into_pruned_query(self) -> SmtQuery {
        let mut used: BTreeSet<String> = BTreeSet::new();
        for a in &self.assertions {
            collect_smt_tokens(a, &mut used);
        }

        // Pull in any `dep_axioms` whose anchor symbol is in `used`, transitively with `pure_defs`.
        let mut kept_pure = vec![false; self.pure_defs.len()];
        let mut kept_dep = vec![false; self.dep_axioms.len()];
        loop {
            let mut progressed = false;
            for (i, (syms, eq)) in self.pure_defs.iter().enumerate() {
                if !kept_pure[i] && syms.iter().any(|s| used.contains(s)) {
                    kept_pure[i] = true;
                    collect_smt_tokens(eq, &mut used);
                    progressed = true;
                }
            }
            for (i, (anchor, ax)) in self.dep_axioms.iter().enumerate() {
                if !kept_dep[i] && used.contains(anchor) {
                    kept_dep[i] = true;
                    collect_smt_tokens(ax, &mut used);
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }

        let mut decls = BTreeMap::new();
        for (name, sort) in self.decls {
            if self.always_keep_decls.contains(&name) || used.contains(&name) {
                decls.insert(name, sort);
            }
        }
        let mut fun_decls = BTreeMap::new();
        for (name, sig) in self.fun_decls {
            if used.contains(&name) {
                fun_decls.insert(name, sig);
            }
        }
        let mut assertions = Vec::new();
        for (i, (_, eq)) in self.pure_defs.into_iter().enumerate() {
            if kept_pure[i] {
                assertions.push(eq);
            }
        }
        for (i, (_, ax)) in self.dep_axioms.into_iter().enumerate() {
            if kept_dep[i] {
                assertions.push(ax);
            }
        }
        assertions.extend(self.assertions);

        SmtQuery {
            decls,
            fun_decls,
            assertions,
        }
    }
}

fn collect_smt_tokens(expr: &str, out: &mut BTreeSet<String>) {
    let bytes = expr.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if is_smt_ident_byte(b) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_smt_ident_byte(bytes[i]) {
                i += 1;
            }
            out.insert(expr[start..i].to_string());
        } else {
            i += 1;
        }
    }
}

fn is_smt_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-'
}

fn coll_in_bounds(sv: &SymVal, flat: &str) -> Option<String> {
    match sv {
        SymVal::Array { len, .. } => Some(format!(
            "(bvult {flat} {})",
            bv_lit_u128(u128::from(*len), 64)
        )),
        SymVal::Tensor { dims, .. } => {
            let total: u64 = dims
                .iter()
                .fold(1u64, |acc, &d| acc.saturating_mul(u64::from(d)));
            Some(format!(
                "(bvult {flat} {})",
                bv_lit_u128(u128::from(total), 64)
            ))
        }
        SymVal::Slice { len, .. } => Some(format!("(bvult {flat} {len})")),
        _ => None,
    }
}

fn match_watch_to_range_vars(
    num_ranges: usize,
    patterns: &[IdxPattern],
    acc: &IndexAccess,
) -> Option<Vec<(String, u32)>> {
    if patterns.len() == 1 && num_ranges == 1 {
        let idx_65 = zext(&acc.flat, 64, 65);
        let res = match &patterns[0] {
            IdxPattern::Var(0) => idx_65,
            IdxPattern::VarPlus(0, off) => {
                let ow = int_literal_width(*off);
                let w = 66u32.max(ow + 1);
                format!("(bvsub {} {})", sext(&idx_65, 65, w), bv_lit_i128(*off, w))
            }
            IdxPattern::ConstMinusVar(c, 0) => {
                let cw = int_literal_width(*c);
                let w = 66u32.max(cw + 1);
                format!("(bvsub {} {})", bv_lit_i128(*c, w), sext(&idx_65, 65, w))
            }
            _ => return None,
        };
        let w = if matches!(&patterns[0], IdxPattern::Var(0)) {
            65
        } else {
            let off_w = match &patterns[0] {
                IdxPattern::VarPlus(_, off) | IdxPattern::ConstMinusVar(off, _) => {
                    int_literal_width(*off) + 1
                }
                _ => 65,
            };
            66u32.max(off_w)
        };
        return Some(vec![(res, w)]);
    }
    if patterns.len() == num_ranges && acc.dims.len() == num_ranges {
        let mut out = vec![None; num_ranges];
        for (pat, d_expr) in patterns.iter().zip(&acc.dims) {
            let idx_65 = zext(d_expr, 64, 65);
            match *pat {
                IdxPattern::Var(p) if out[p].is_none() => out[p] = Some((idx_65, 65)),
                IdxPattern::VarPlus(p, off) if out[p].is_none() => {
                    let w = 66u32.max(int_literal_width(off) + 1);
                    out[p] = Some((
                        format!("(bvsub {} {})", sext(&idx_65, 65, w), bv_lit_i128(off, w)),
                        w,
                    ));
                }
                IdxPattern::ConstMinusVar(c, p) if out[p].is_none() => {
                    let w = 66u32.max(int_literal_width(c) + 1);
                    out[p] = Some((
                        format!("(bvsub {} {})", bv_lit_i128(c, w), sext(&idx_65, 65, w)),
                        w,
                    ));
                }
                _ => return None,
            }
        }
        return out.into_iter().collect();
    }
    None
}

impl SavedEnv {
    fn as_env<'a>(&'a self, callee: &'a dyn Fn(&Hash) -> Option<Node>) -> FormulaEnv<'a> {
        FormulaEnv {
            node: &self.node,
            callee,
            inputs: self.inputs.clone(),
            results: self.results.clone(),
            ports: self.ports.clone(),
            bound: self.bound.clone(),
            resources: self.resources.clone(),
        }
    }
}

/// Evaluation environment for contract formulas.
#[derive(Clone)]
pub struct FormulaEnv<'a> {
    pub node: &'a Node,
    pub callee: &'a dyn Fn(&Hash) -> Option<Node>,
    pub inputs: Vec<(Type, SymVal)>,
    pub results: Vec<(Type, SymVal)>,
    pub ports: BTreeMap<(u32, u8), (Type, SymVal)>,
    pub bound: Vec<FVal>,
    pub resources: Option<CostResources>,
}

impl<'a> FormulaEnv<'a> {
    pub fn snapshot(&self) -> SavedEnv {
        SavedEnv {
            node: self.node.clone(),
            inputs: self.inputs.clone(),
            results: self.results.clone(),
            ports: self.ports.clone(),
            bound: self.bound.clone(),
            resources: self.resources.clone(),
        }
    }
}

/// Evaluates constant integer terms statically if possible.
pub fn eval_const_int(t: &Term, node: &Node) -> Option<i128> {
    match t {
        Term::Const(c) => {
            let c = &node.consts[*c as usize];
            if c.ty == Type::Int {
                int_value(&c.bytes)
            } else {
                None
            }
        }
        Term::Neg(a) => eval_const_int(a, node)?.checked_neg(),
        Term::Bin(Bin::Add, a, b) => eval_const_int(a, node)?.checked_add(eval_const_int(b, node)?),
        Term::Bin(Bin::Sub, a, b) => eval_const_int(a, node)?.checked_sub(eval_const_int(b, node)?),
        Term::Bin(Bin::Mul, a, b) => eval_const_int(a, node)?.checked_mul(eval_const_int(b, node)?),
        _ => None,
    }
}

fn eval_int_bin(op: Bin, a: FVal, b: FVal, qb: &mut QueryBuilder) -> FVal {
    let (ae, aw) = a.into_int().expect("int lhs");
    let (be, bw) = b.into_int().expect("int rhs");
    match op {
        Bin::Add => {
            let w = aw.max(bw) + 1;
            FVal::Int {
                expr: format!("(bvadd {} {})", sext(&ae, aw, w), sext(&be, bw, w)),
                width: w,
                orig: None,
            }
        }
        Bin::Sub => {
            let w = aw.max(bw) + 1;
            FVal::Int {
                expr: format!("(bvsub {} {})", sext(&ae, aw, w), sext(&be, bw, w)),
                width: w,
                orig: None,
            }
        }
        Bin::Mul => {
            let w = aw + bw;
            FVal::Int {
                expr: format!("(bvmul {} {})", sext(&ae, aw, w), sext(&be, bw, w)),
                width: w,
                orig: None,
            }
        }
        Bin::Div => {
            let w_op = aw.max(bw);
            let uf = format!("fdiv_zero_{aw}");
            qb.declare_fun_once(
                &uf,
                vec![format!("(_ BitVec {aw})")],
                format!("(_ BitVec {aw})"),
            );
            let a_ext = sext(&ae, aw, w_op);
            let b_ext = sext(&be, bw, w_op);
            let q = extract_lo(&format!("(bvsdiv {a_ext} {b_ext})"), w_op, aw);
            FVal::Int {
                expr: format!("(ite (= {b_ext} {}) ({uf} {ae}) {q})", bv_lit_u128(0, w_op)),
                width: aw,
                orig: None,
            }
        }
        Bin::Mod => {
            let w_op = aw.max(bw);
            let w_res = aw.min(bw);
            let uf = format!("fmod_zero_{aw}_{w_res}");
            qb.declare_fun_once(
                &uf,
                vec![format!("(_ BitVec {aw})")],
                format!("(_ BitVec {w_res})"),
            );
            let a_ext = sext(&ae, aw, w_op);
            let b_ext = sext(&be, bw, w_op);
            let r = extract_lo(&format!("(bvsrem {a_ext} {b_ext})"), w_op, w_res);
            FVal::Int {
                expr: format!("(ite (= {b_ext} {}) ({uf} {ae}) {r})", bv_lit_u128(0, w_op)),
                width: w_res,
                orig: None,
            }
        }
        _ => unreachable!("eval_int_bin only handles arithmetic"),
    }
}

fn eval_int_cmp(op: Bin, a: FVal, b: FVal) -> String {
    let (ae, aw) = a.into_int().expect("int lhs");
    let (be, bw) = b.into_int().expect("int rhs");
    let w = aw.max(bw);
    let as_ = sext(&ae, aw, w);
    let bs_ = sext(&be, bw, w);
    match op {
        Bin::Eq => format!("(= {as_} {bs_})"),
        Bin::Ne => format!("(not (= {as_} {bs_}))"),
        Bin::Lt => format!("(bvslt {as_} {bs_})"),
        Bin::Le => format!("(bvsle {as_} {bs_})"),
        Bin::Gt => format!("(bvsgt {as_} {bs_})"),
        Bin::Ge => format!("(bvsge {as_} {bs_})"),
        _ => unreachable!("not a comparison"),
    }
}

/// Evaluates a formula `Term` into an `FVal` (assuming quantifier-free or Boolean contexts).
pub fn eval_term(t: &Term, env: &mut FormulaEnv<'_>, qb: &mut QueryBuilder) -> FVal {
    eval_term_depth(t, env, qb, 0)
}

fn eval_term_depth(
    t: &Term,
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> FVal {
    match t {
        Term::Input(i) => {
            let (ty, sv) = &env.inputs[*i as usize];
            symval_to_fval(sv, ty)
        }
        Term::Result(j) => {
            let (ty, sv) = &env.results[*j as usize];
            symval_to_fval(sv, ty)
        }
        Term::Port(i, s) => {
            let (ty, sv) = &env.ports[&(*i, *s)];
            symval_to_fval(sv, ty)
        }
        Term::Bound(d) => {
            let idx = env.bound.len() - 1 - (*d as usize);
            env.bound[idx].clone()
        }
        Term::Const(c) => {
            let cnst = &env.node.consts[*c as usize];
            if cnst.ty == Type::Int {
                let v = int_value(&cnst.bytes).expect("valid Int const");
                let w = int_literal_width(v);
                FVal::Int {
                    expr: bv_lit_i128(v, w),
                    width: w,
                    orig: None,
                }
            } else {
                let hint = format!("fc{c}");
                let sv = const_to_symval(
                    cnst,
                    &hint,
                    &mut qb.decls,
                    &mut qb.assertions,
                    &mut qb.next_coll_id,
                );
                qb.register_coll_id(&sv);
                symval_to_fval(&sv, &cnst.ty)
            }
        }
        Term::Resource(r) => match r {
            Resource::Ops => env
                .resources
                .as_ref()
                .map(|rs| rs.ops.clone())
                .unwrap_or_else(|| {
                    qb.unresolved_error
                        .get_or_insert_with(|| "missing ops resource model".into());
                    FVal::Int {
                        expr: bv_lit_i128(0, 65),
                        width: 65,
                        orig: None,
                    }
                }),
            Resource::ArenaBytes => env
                .resources
                .as_ref()
                .map(|rs| rs.arena_bytes.clone())
                .unwrap_or_else(|| {
                    qb.unresolved_error
                        .get_or_insert_with(|| "missing arena_bytes resource model".into());
                    FVal::Int {
                        expr: bv_lit_i128(0, 65),
                        width: 65,
                        orig: None,
                    }
                }),
            Resource::HeapBytes => env
                .resources
                .as_ref()
                .map(|rs| rs.heap_bytes.clone())
                .unwrap_or_else(|| FVal::Int {
                    expr: bv_lit_i128(0, 2),
                    width: 2,
                    orig: None,
                }),
            Resource::StackBytes | Resource::LatencyP99Us => {
                qb.unresolved_error.get_or_insert_with(|| {
                    format!("resource `{}` is not statically provable in MVP", r.name())
                });
                let sym = qb.fresh_sym("unprovable_res", "(_ BitVec 65)");
                FVal::Int {
                    expr: sym,
                    width: 65,
                    orig: None,
                }
            }
        },
        Term::Not(a) => {
            let inner = encode_neg_goal_depth(a, env, qb, unfold_depth);
            FVal::Bool(inner)
        }
        Term::Neg(a) => match eval_term_depth(a, env, qb, unfold_depth) {
            FVal::Int { expr, width, .. } => FVal::Int {
                expr: format!("(bvneg {expr})"),
                width,
                orig: None,
            },
            FVal::Float { expr, ty } => FVal::Float {
                expr: format!("(fp.neg {expr})"),
                ty,
            },
            other => panic!("Neg on non-numeric {other:?}"),
        },
        Term::Bin(op, a, b) => match op {
            Bin::And => FVal::Bool(encode_assume_depth(t, env, qb, unfold_depth)),
            Bin::Or => FVal::Bool(encode_assume_depth(t, env, qb, unfold_depth)),
            Bin::Implies => FVal::Bool(encode_assume_depth(t, env, qb, unfold_depth)),
            Bin::Eq | Bin::Ne => {
                let va = eval_term_depth(a, env, qb, unfold_depth);
                let vb = eval_term_depth(b, env, qb, unfold_depth);
                let eq = match (&va, &vb) {
                    (FVal::Bool(_), FVal::Bool(_)) => {
                        if contains_forall(a) || contains_forall(b) {
                            let eq_t = expand_bool_eq(*op == Bin::Eq, a, b);
                            return FVal::Bool(encode_assume_depth(&eq_t, env, qb, unfold_depth));
                        }
                        format!("(= {} {})", va.as_bool().unwrap(), vb.as_bool().unwrap())
                    }
                    (FVal::Int { .. }, FVal::Int { .. }) => {
                        return FVal::Bool(eval_int_cmp(*op, va, vb));
                    }
                    (FVal::Float { expr: fa, .. }, FVal::Float { expr: fb, .. }) => {
                        format!("(fp.eq {fa} {fb})")
                    }
                    (FVal::Val { val: sa, ty }, FVal::Val { val: sb, .. }) => {
                        if *op == Bin::Eq {
                            encode_val_eq_assume(ty, sa, sb, false, env, qb)
                        } else {
                            let eq_b = encode_val_eq_goal_neg(ty, sa, sb, false, env, qb);
                            return FVal::Bool(eq_b);
                        }
                    }
                    _ => panic!("mismatched == operands: {va:?} vs {vb:?}"),
                };
                if *op == Bin::Ne {
                    FVal::Bool(format!("(not {eq})"))
                } else {
                    FVal::Bool(eq)
                }
            }
            Bin::Lt | Bin::Le | Bin::Gt | Bin::Ge => {
                let va = eval_term_depth(a, env, qb, unfold_depth);
                let vb = eval_term_depth(b, env, qb, unfold_depth);
                match (&va, &vb) {
                    (FVal::Int { .. }, FVal::Int { .. }) => FVal::Bool(eval_int_cmp(*op, va, vb)),
                    (FVal::Float { expr: fa, .. }, FVal::Float { expr: fb, .. }) => {
                        let fop = match op {
                            Bin::Lt => "fp.lt",
                            Bin::Le => "fp.leq",
                            Bin::Gt => "fp.gt",
                            Bin::Ge => "fp.geq",
                            _ => unreachable!(),
                        };
                        FVal::Bool(format!("({fop} {fa} {fb})"))
                    }
                    _ => panic!("mismatched cmp operands: {va:?} vs {vb:?}"),
                }
            }
            Bin::Add | Bin::Sub | Bin::Mul | Bin::Div | Bin::Mod => {
                let va = eval_term_depth(a, env, qb, unfold_depth);
                let vb = eval_term_depth(b, env, qb, unfold_depth);
                match (&va, &vb) {
                    (FVal::Int { .. }, FVal::Int { .. }) => eval_int_bin(*op, va, vb, qb),
                    (FVal::Float { expr: fa, ty }, FVal::Float { expr: fb, .. }) => {
                        let fop = match op {
                            Bin::Add => "fp.add RNE",
                            Bin::Sub => "fp.sub RNE",
                            Bin::Mul => "fp.mul RNE",
                            Bin::Div => "fp.div RNE",
                            Bin::Mod => {
                                qb.unresolved_error
                                    .get_or_insert_with(|| "float modulo not supported".into());
                                return va;
                            }
                            _ => unreachable!(),
                        };
                        FVal::Float {
                            expr: format!("({fop} {fa} {fb})"),
                            ty: ty.clone(),
                        }
                    }
                    _ => panic!("mismatched arith operands: {va:?} vs {vb:?}"),
                }
            }
        },
        Term::Index(base, idx) => {
            let FVal::Val { val: coll, ty } = eval_term_depth(base, env, qb, unfold_depth) else {
                panic!("Index base must be collection");
            };
            let idx_vals: Vec<(String, u32)> = idx
                .iter()
                .map(|i| {
                    eval_term_depth(i, env, qb, unfold_depth)
                        .into_int()
                        .expect("int index")
                })
                .collect();
            eval_index_access(&coll, &ty, &idx_vals, qb)
        }
        Term::Proj(base, j) => {
            let FVal::Val { val, .. } = eval_term_depth(base, env, qb, unfold_depth) else {
                panic!("Proj base must be Tuple");
            };
            let SymVal::Tuple { elems } = val else {
                panic!("Proj base is not Tuple: {val:?}");
            };
            let (ty, sv) = &elems[*j as usize];
            symval_to_fval(sv, ty)
        }
        Term::Forall(..) => FVal::Bool(encode_assume_depth(t, env, qb, unfold_depth)),
        Term::Builtin(b, args) => eval_builtin(*b, args, env, qb, unfold_depth),
        Term::Spec(c, args) => eval_spec(*c, args, env, qb, unfold_depth),
        Term::Fold(c, args) => eval_fold(*c, args, env, qb, unfold_depth),
        Term::Apply(c, args) => eval_apply(*c, args, env, qb, unfold_depth),
    }
}

fn eval_index_access(
    coll: &SymVal,
    ty: &Type,
    idx_vals: &[(String, u32)],
    qb: &mut QueryBuilder,
) -> FVal {
    match ty {
        Type::Array(elem_ty, len) => {
            let (ie, iw) = &idx_vals[0];
            let cw = (*iw).max(65);
            let i_ext = sext(ie, *iw, cw);
            let in_bounds = format!(
                "(and (bvsge {i_ext} {}) (bvslt {i_ext} {}))",
                bv_lit_i128(0, cw),
                bv_lit_i128(i128::from(*len), cw)
            );
            let idx_bv64 = extract_lo(&i_ext, cw, 64);
            qb.record_index(coll, vec![idx_bv64.clone()], idx_bv64.clone());
            let selected = symval_select(coll, &idx_bv64, &mut qb.next_coll_id);
            qb.register_coll_id(&selected);
            if **elem_ty == Type::Bool || elem_ty.is_int() || elem_ty.is_float() {
                let oob = qb.fresh_symval("idx_oob", elem_ty);
                let guarded =
                    symval_ite(&in_bounds, elem_ty, &selected, &oob, &mut qb.next_coll_id);
                qb.register_coll_id(&guarded);
                symval_to_fval(&guarded, elem_ty)
            } else {
                symval_to_fval(&selected, elem_ty)
            }
        }
        Type::Slice(elem_ty) => {
            let SymVal::Slice { len, .. } = coll else {
                unreachable!();
            };
            let (ie, iw) = &idx_vals[0];
            let cw = (*iw).max(65);
            let i_ext = sext(ie, *iw, cw);
            let len_ext = zext(len, 64, cw);
            let in_bounds = format!(
                "(and (bvsge {i_ext} {}) (bvslt {i_ext} {len_ext}))",
                bv_lit_i128(0, cw)
            );
            let idx_bv64 = extract_lo(&i_ext, cw, 64);
            qb.record_index(coll, vec![idx_bv64.clone()], idx_bv64.clone());
            let selected = symval_select(coll, &idx_bv64, &mut qb.next_coll_id);
            qb.register_coll_id(&selected);
            if **elem_ty == Type::Bool || elem_ty.is_int() || elem_ty.is_float() {
                let oob = qb.fresh_symval("idx_oob", elem_ty);
                let guarded =
                    symval_ite(&in_bounds, elem_ty, &selected, &oob, &mut qb.next_coll_id);
                qb.register_coll_id(&guarded);
                symval_to_fval(&guarded, elem_ty)
            } else {
                symval_to_fval(&selected, elem_ty)
            }
        }
        Type::Tensor(elem_ty, dims) => {
            let mut in_bounds_parts = Vec::with_capacity(dims.len());
            let mut dim_bv64s = Vec::with_capacity(dims.len());
            let mut flat = bv_lit_u128(0, 64);
            for (k, (&(ref ie, iw), &d)) in idx_vals.iter().zip(dims).enumerate() {
                let cw = iw.max(65);
                let i_ext = sext(ie, iw, cw);
                in_bounds_parts.push(format!(
                    "(and (bvsge {i_ext} {}) (bvslt {i_ext} {}))",
                    bv_lit_i128(0, cw),
                    bv_lit_i128(i128::from(d), cw)
                ));
                let i_64 = extract_lo(&i_ext, cw, 64);
                dim_bv64s.push(i_64.clone());
                if k == 0 {
                    flat = i_64;
                } else {
                    flat = format!(
                        "(bvadd (bvmul {flat} {}) {i_64})",
                        bv_lit_u128(u128::from(d), 64)
                    );
                }
            }
            let in_bounds = if in_bounds_parts.len() == 1 {
                in_bounds_parts.remove(0)
            } else {
                format!("(and {})", in_bounds_parts.join(" "))
            };
            qb.record_index(coll, dim_bv64s, flat.clone());
            let selected = symval_select(coll, &flat, &mut qb.next_coll_id);
            qb.register_coll_id(&selected);
            if **elem_ty == Type::Bool || elem_ty.is_int() || elem_ty.is_float() {
                let oob = qb.fresh_symval("idx_oob", elem_ty);
                let guarded =
                    symval_ite(&in_bounds, elem_ty, &selected, &oob, &mut qb.next_coll_id);
                qb.register_coll_id(&guarded);
                symval_to_fval(&guarded, elem_ty)
            } else {
                symval_to_fval(&selected, elem_ty)
            }
        }
        _ => panic!("Index on non-collection {ty}"),
    }
}

fn eval_builtin(
    b: Builtin,
    args: &[Term],
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> FVal {
    match b {
        Builtin::Len => {
            let FVal::Val { val, .. } = eval_term_depth(&args[0], env, qb, unfold_depth) else {
                panic!("len arg must be collection");
            };
            match val {
                SymVal::Array { len, .. } => FVal::Int {
                    expr: bv_lit_i128(i128::from(len), 65),
                    width: 65,
                    orig: None,
                },
                SymVal::Tensor { dims, .. } => {
                    let total: i128 = dims.iter().map(|&d| i128::from(d)).product();
                    FVal::Int {
                        expr: bv_lit_i128(total, 65),
                        width: 65,
                        orig: None,
                    }
                }
                SymVal::Slice { len, .. } => FVal::Int {
                    expr: zext(&len, 64, 65),
                    width: 65,
                    orig: None,
                },
                _ => panic!("len on non-collection {val:?}"),
            }
        }
        Builtin::IsFinite => {
            let FVal::Float { expr, .. } = eval_term_depth(&args[0], env, qb, unfold_depth) else {
                panic!("is_finite expects float");
            };
            FVal::Bool(format!(
                "(and (not (fp.isNaN {expr})) (not (fp.isInfinite {expr})))"
            ))
        }
        Builtin::IsNan => {
            let FVal::Float { expr, .. } = eval_term_depth(&args[0], env, qb, unfold_depth) else {
                panic!("is_nan expects float");
            };
            FVal::Bool(format!("(fp.isNaN {expr})"))
        }
        Builtin::Abs => match eval_term_depth(&args[0], env, qb, unfold_depth) {
            FVal::Int { expr, width, .. } => FVal::Int {
                expr: format!(
                    "(ite (bvslt {expr} {}) (bvneg {expr}) {expr})",
                    bv_lit_u128(0, width)
                ),
                width,
                orig: None,
            },
            FVal::Float { expr, ty } => FVal::Float {
                expr: format!("(fp.abs {expr})"),
                ty,
            },
            _ => panic!("abs expects int or float"),
        },
        Builtin::Min | Builtin::Max => {
            let va = eval_term_depth(&args[0], env, qb, unfold_depth);
            let vb = eval_term_depth(&args[1], env, qb, unfold_depth);
            match (va, vb) {
                (
                    FVal::Int {
                        expr: ae,
                        width: aw,
                        ..
                    },
                    FVal::Int {
                        expr: be,
                        width: bw,
                        ..
                    },
                ) => {
                    let w = aw.max(bw);
                    let as_ = sext(&ae, aw, w);
                    let bs_ = sext(&be, bw, w);
                    let cmp = if b == Builtin::Min { "bvslt" } else { "bvsgt" };
                    FVal::Int {
                        expr: format!("(ite ({cmp} {as_} {bs_}) {as_} {bs_})"),
                        width: w,
                        orig: None,
                    }
                }
                (FVal::Float { expr: fa, ty }, FVal::Float { expr: fb, .. }) => {
                    let (eb, sb) = fp_format(&ty).expect("float");
                    let expr = if b == Builtin::Min {
                        smt_fmin(&fa, &fb, eb, sb)
                    } else {
                        smt_fmax(&fa, &fb, eb, sb)
                    };
                    FVal::Float { expr, ty }
                }
                _ => panic!("min/max type mismatch"),
            }
        }
        Builtin::AsBytes => {
            let v = eval_term_depth(&args[0], env, qb, unfold_depth);
            let (sv, ty) = match v {
                FVal::Bool(e) => (SymVal::Bool(e), Type::Bool),
                FVal::Float { expr, ty } => (
                    SymVal::Float {
                        expr,
                        ty: ty.clone(),
                    },
                    ty,
                ),
                FVal::Int {
                    orig: Some((ty, expr)),
                    ..
                } => (
                    SymVal::Int {
                        expr,
                        ty: ty.clone(),
                    },
                    ty,
                ),
                FVal::Val { val, ty } => (val, ty),
                other => {
                    qb.unresolved_error.get_or_insert_with(|| {
                        format!("as_bytes on untyped formula value {other:?}")
                    });
                    let sv = qb.fresh_symval("as_bytes_err", &Type::Slice(Box::new(Type::UInt(8))));
                    return FVal::Val {
                        val: sv,
                        ty: Type::Slice(Box::new(Type::UInt(8))),
                    };
                }
            };
            let slice = encode_as_bytes(&ty, &sv, qb);
            FVal::Val {
                val: slice,
                ty: Type::Slice(Box::new(Type::UInt(8))),
            }
        }
        Builtin::Same => eval_same_builtin(args, env, qb, unfold_depth),
        Builtin::IsOk => {
            let FVal::Val {
                val: SymVal::Result { is_ok, .. },
                ..
            } = eval_term_depth(&args[0], env, qb, unfold_depth)
            else {
                panic!("is_ok expects Result");
            };
            FVal::Bool(is_ok)
        }
        Builtin::Ok => {
            let FVal::Val {
                val: SymVal::Result {
                    is_ok, ok_ty, ok, ..
                },
                ..
            } = eval_term_depth(&args[0], env, qb, unfold_depth)
            else {
                panic!("ok expects Result");
            };
            let oob = qb.fresh_symval("ok_undef", &ok_ty);
            let guarded = symval_ite(&is_ok, &ok_ty, &ok, &oob, &mut qb.next_coll_id);
            qb.register_coll_id(&guarded);
            symval_to_fval(&guarded, &ok_ty)
        }
        Builtin::Err => {
            let FVal::Val {
                val: SymVal::Result {
                    is_ok, err_ty, err, ..
                },
                ..
            } = eval_term_depth(&args[0], env, qb, unfold_depth)
            else {
                panic!("err expects Result");
            };
            let oob = qb.fresh_symval("err_undef", &err_ty);
            let guarded = symval_ite(
                &format!("(not {is_ok})"),
                &err_ty,
                &err,
                &oob,
                &mut qb.next_coll_id,
            );
            qb.register_coll_id(&guarded);
            symval_to_fval(&guarded, &err_ty)
        }
    }
}

fn eval_same_builtin(
    args: &[Term],
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> FVal {
    let va = eval_term_depth(&args[0], env, qb, unfold_depth);
    let vb = eval_term_depth(&args[1], env, qb, unfold_depth);
    match (&va, &vb) {
        (FVal::Bool(a), FVal::Bool(b)) => FVal::Bool(format!("(= {a} {b})")),
        (FVal::Int { .. }, FVal::Int { .. }) => FVal::Bool(eval_int_cmp(Bin::Eq, va, vb)),
        (FVal::Float { expr: fa, .. }, FVal::Float { expr: fb, .. }) => FVal::Bool(format!(
            "(or (and (fp.isNaN {fa}) (fp.isNaN {fb})) \
             (and (fp.eq {fa} {fb}) (= (fp.isNegative {fa}) (fp.isNegative {fb}))))"
        )),
        (FVal::Val { val: sa, ty }, FVal::Val { val: sb, .. }) => {
            FVal::Bool(encode_val_eq_assume(ty, sa, sb, true, env, qb))
        }
        _ => panic!("same type mismatch: {va:?} vs {vb:?}"),
    }
}

/// Converts a scalar `SymVal` expression string to a flat little-endian bitvector expression.
fn scalar_to_bv_expr(ty: &Type, expr: &str, qb: &mut QueryBuilder) -> String {
    match ty {
        Type::Bool => format!("(ite {expr} #x01 #x00)"),
        Type::UInt(_) | Type::SInt(_) => expr.to_string(),
        Type::Float(_) | Type::BFloat16 => {
            let (eb, sb) = fp_format(ty).expect("float");
            let w = eb + sb;
            let tag = ty.scalar_name().expect("float name");
            let uf = format!("fp_to_bv_{tag}");
            qb.declare_fun_once(
                &uf,
                vec![format!("(_ FloatingPoint {eb} {sb})")],
                format!("(_ BitVec {w})"),
            );
            let app = format!("({uf} {expr})");
            let key = format!("{uf}::{expr}");
            if qb.fp_to_bv_apps.insert(key) {
                let canon = canonical_nan_bv(ty);
                let ax = format!(
                    "(ite (fp.isNaN {expr}) (= {app} {canon}) (= ((_ to_fp {eb} {sb}) {app}) {expr}))"
                );
                qb.dep_axioms.push((uf.clone(), ax));
            }
            app
        }
        _ => panic!("scalar_to_bv_expr called on {ty}"),
    }
}

fn extract_le_byte(bv_expr: &str, byte_count: u64, byte_idx_bv64: &str) -> String {
    if byte_count == 1 {
        return bv_expr.to_string();
    }
    let mut cur = format!("((_ extract 7 0) {bv_expr})");
    for b in (1..byte_count).rev() {
        let hi = (b as u32) * 8 + 7;
        let lo = (b as u32) * 8;
        let slice = format!("((_ extract {hi} {lo}) {bv_expr})");
        let lit = bv_lit_u128(u128::from(b), 64);
        cur = format!("(ite (= {byte_idx_bv64} {lit}) {slice} {cur})");
    }
    cur
}

/// Encodes `AS_BYTES(v)` / formula `as_bytes(v)` into a `SymVal::Slice<u8>` deterministically.
pub fn encode_as_bytes(ty: &Type, val: &SymVal, qb: &mut QueryBuilder) -> SymVal {
    match ty {
        Type::Bool => {
            let SymVal::Bool(b) = val else { unreachable!() };
            qb.declare_fun_once(
                "bool_to_bytes",
                vec!["Bool".into()],
                "(Array (_ BitVec 64) (_ BitVec 8))".into(),
            );
            let arr = format!("(bool_to_bytes {b})");
            let ax = format!("(= (select {arr} #x0000000000000000) (ite {b} #x01 #x00))");
            qb.dep_axioms.push(("bool_to_bytes".into(), ax));
            let len = bv_lit_u128(1, 64);
            let cid = qb.intern_coll(Some(len.clone()), std::slice::from_ref(&arr));
            SymVal::Slice {
                coll_id: cid,
                len,
                elem_ty: Type::UInt(8),
                arrays: vec![arr],
            }
        }
        Type::UInt(w) | Type::SInt(w) => {
            let SymVal::Int { expr, .. } = val else {
                unreachable!()
            };
            let k = u64::from(*w) / 8;
            let uf = format!("bv{w}_to_bytes");
            qb.declare_fun_once(
                &uf,
                vec![format!("(_ BitVec {w})")],
                "(Array (_ BitVec 64) (_ BitVec 8))".into(),
            );
            let arr = format!("({uf} {expr})");
            for b in 0..k {
                let hi = (b as u32) * 8 + 7;
                let lo = (b as u32) * 8;
                let ax = format!(
                    "(= (select {arr} {}) ((_ extract {hi} {lo}) {expr}))",
                    bv_lit_u128(u128::from(b), 64)
                );
                qb.dep_axioms.push((uf.clone(), ax));
            }
            let len = bv_lit_u128(u128::from(k), 64);
            let cid = qb.intern_coll(Some(len.clone()), std::slice::from_ref(&arr));
            SymVal::Slice {
                coll_id: cid,
                len,
                elem_ty: Type::UInt(8),
                arrays: vec![arr],
            }
        }
        Type::Float(_) | Type::BFloat16 => {
            let SymVal::Float { expr, .. } = val else {
                unreachable!()
            };
            let w = fp_bit_width(ty).expect("float width");
            let k = u64::from(w) / 8;
            let bv = scalar_to_bv_expr(ty, expr, qb);
            let uf = format!("bv{w}_to_bytes");
            qb.declare_fun_once(
                &uf,
                vec![format!("(_ BitVec {w})")],
                "(Array (_ BitVec 64) (_ BitVec 8))".into(),
            );
            let arr = format!("({uf} {bv})");
            for b in 0..k {
                let hi = (b as u32) * 8 + 7;
                let lo = (b as u32) * 8;
                let ax = format!(
                    "(= (select {arr} {}) ((_ extract {hi} {lo}) {bv}))",
                    bv_lit_u128(u128::from(b), 64)
                );
                qb.dep_axioms.push((uf.clone(), ax));
            }
            let len = bv_lit_u128(u128::from(k), 64);
            let cid = qb.intern_coll(Some(len.clone()), std::slice::from_ref(&arr));
            SymVal::Slice {
                coll_id: cid,
                len,
                elem_ty: Type::UInt(8),
                arrays: vec![arr],
            }
        }
        Type::Array(elem, _) | Type::Tensor(elem, _) | Type::Slice(elem) => {
            let elem_ty = (**elem).clone();
            let src_arrays = val.arrays().expect("collection arrays");
            let src_len_bv64 = match val {
                SymVal::Array { len, .. } => bv_lit_u128(u128::from(*len), 64),
                SymVal::Tensor { dims, .. } => {
                    let tot: u64 = dims
                        .iter()
                        .fold(1u64, |a, &d| a.saturating_mul(u64::from(d)));
                    bv_lit_u128(u128::from(tot), 64)
                }
                SymVal::Slice { len, .. } => len.clone(),
                _ => unreachable!(),
            };
            if elem_ty == Type::UInt(8) {
                let cid = qb.intern_coll(Some(src_len_bv64.clone()), src_arrays);
                if let Some(src_cid) = val.coll_id()
                    && src_cid != cid
                {
                    qb.coll_links.push(CollLink::Identity {
                        dst: cid,
                        src: src_cid,
                    });
                }
                return SymVal::Slice {
                    coll_id: cid,
                    len: src_len_bv64,
                    elem_ty: Type::UInt(8),
                    arrays: src_arrays.to_vec(),
                };
            }
            let elem_bytes = elem_ty
                .byte_size()
                .expect("MVP AS_BYTES collection holds scalar or Bool elements");
            let total_bytes_bv64 = match val {
                SymVal::Array { len, .. } => {
                    bv_lit_u128(u128::from(*len) * u128::from(elem_bytes), 64)
                }
                SymVal::Tensor { dims, .. } => {
                    let tot: u128 = dims.iter().map(|&d| u128::from(d)).product();
                    bv_lit_u128(tot * u128::from(elem_bytes), 64)
                }
                SymVal::Slice { len, .. } => {
                    format!("(bvmul {len} {})", bv_lit_u128(u128::from(elem_bytes), 64))
                }
                _ => unreachable!(),
            };
            let elem_sort = flatten_type(&elem_ty)[0].1.to_string();
            let tag = ty_short_tag(&elem_ty);
            let uf = format!("as_bytes_arr_{tag}");
            qb.declare_fun_once(
                &uf,
                vec![format!("(Array (_ BitVec 64) {elem_sort})")],
                "(Array (_ BitVec 64) (_ BitVec 8))".into(),
            );
            let src_arr = src_arrays[0].clone();
            let dst_arr = format!("({uf} {src_arr})");
            let cid = qb.intern_coll(
                Some(total_bytes_bv64.clone()),
                std::slice::from_ref(&dst_arr),
            );
            if let Some(src_cid) = val.coll_id() {
                qb.coll_links.push(CollLink::AsBytes {
                    dst: cid,
                    src: src_cid,
                    elem_bytes,
                });
            }
            qb.pending_schemas.push(ElemSchema::AsBytes {
                dst_coll: cid,
                dst_arr: dst_arr.clone(),
                src_arr,
                src_elem_ty: elem_ty,
                elem_bytes,
                total_bytes_bv64: total_bytes_bv64.clone(),
            });
            SymVal::Slice {
                coll_id: cid,
                len: total_bytes_bv64,
                elem_ty: Type::UInt(8),
                arrays: vec![dst_arr],
            }
        }
        _ => {
            qb.unresolved_error
                .get_or_insert_with(|| format!("AS_BYTES on unsupported type {ty}"));
            qb.fresh_symval("as_bytes_bad", &Type::Slice(Box::new(Type::UInt(8))))
        }
    }
}

pub fn ty_short_tag(ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".to_string(),
        Type::UInt(w) => format!("u{w}"),
        Type::SInt(w) => format!("i{w}"),
        Type::Float(w) => format!("f{w}"),
        Type::BFloat16 => "bf16".to_string(),
        Type::Handle(h) => format!("h_{}", h.name()),
        Type::Array(e, n) => format!("arr{n}_{}", ty_short_tag(e)),
        Type::Tensor(e, ds) => {
            let d = ds
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join("x");
            format!("ten{d}_{}", ty_short_tag(e))
        }
        Type::Slice(e) => format!("slc_{}", ty_short_tag(e)),
        Type::Tuple(ms) => {
            let m = ms.iter().map(ty_short_tag).collect::<Vec<_>>().join("_");
            format!("tup_{m}")
        }
        Type::Result(a, b) => format!("res_{}_{}", ty_short_tag(a), ty_short_tag(b)),
        _ => "other".to_string(),
    }
}

fn eval_spec(
    call_ix: u32,
    args: &[Term],
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> FVal {
    let spec_hash = env.node.calls[call_ix as usize];
    let Some(spec_node) = (env.callee)(&spec_hash) else {
        qb.unresolved_error.get_or_insert_with(|| {
            format!(
                "missing SPEC node {} for spec: call",
                vg_core::ir::hex(&spec_hash)
            )
        });
        return FVal::Bool("false".to_string());
    };
    let mut arg_vals = Vec::with_capacity(args.len());
    for (a, p_ty) in args.iter().zip(&spec_node.params) {
        let fv = eval_term_depth(a, env, qb, unfold_depth);
        let sv = fval_to_symval(&fv, p_ty).expect("spec arg type matches");
        arg_vals.push((p_ty.clone(), sv));
    }
    let res_ty = if spec_node.results.len() == 1 {
        spec_node.results[0].clone()
    } else {
        Type::Tuple(spec_node.results.clone())
    };
    let mut flat_arg_sorts = Vec::new();
    let mut flat_arg_atoms = Vec::new();
    for (t, sv) in &arg_vals {
        for (_, s) in flatten_type(t) {
            flat_arg_sorts.push(s.to_string());
        }
        flat_arg_atoms.extend(symval_atoms(sv));
    }
    let hex12 = &vg_core::ir::hex(&spec_hash)[..12];
    let out_sorts = flatten_type(&res_ty);
    let mut out_atoms = Vec::with_capacity(out_sorts.len());
    for (k, (_, s)) in out_sorts.into_iter().enumerate() {
        let uf = format!("spec_{hex12}_{k}");
        qb.declare_fun_once(&uf, flat_arg_sorts.clone(), s.to_string());
        if flat_arg_atoms.is_empty() {
            out_atoms.push(uf);
        } else {
            out_atoms.push(format!("({uf} {})", flat_arg_atoms.join(" ")));
        }
    }
    let res_sym = symval_from_atoms(&res_ty, &out_atoms, &mut qb.next_coll_id);
    qb.register_coll_id(&res_sym);

    let app_key = format!("{hex12}:{}", flat_arg_atoms.join(","));
    if qb.spec_apps.insert(app_key) {
        // Soundness Rule 2: EVERY fact of `spec:` (POST and 1-level unfolding) is guarded by PRE_spec!
        let mut s_env = FormulaEnv {
            node: &spec_node,
            callee: env.callee,
            inputs: arg_vals.clone(),
            results: match &spec_node.results[..] {
                [t] => vec![(t.clone(), res_sym.clone())],
                ts => {
                    let SymVal::Tuple { elems } = &res_sym else {
                        unreachable!()
                    };
                    ts.iter()
                        .zip(elems)
                        .map(|(t, (_, v))| (t.clone(), v.clone()))
                        .collect()
                }
            },
            ports: BTreeMap::new(),
            bound: Vec::new(),
            resources: None,
        };
        let neg_pre = if spec_node.contract.pre.is_empty() {
            None
        } else {
            let neg_parts: Vec<String> = spec_node
                .contract
                .pre
                .iter()
                .map(|p| encode_neg_goal_depth(p, &mut s_env, qb, unfold_depth + 1))
                .collect();
            Some(if neg_parts.len() == 1 {
                neg_parts.into_iter().next().expect("1")
            } else {
                format!("(or {})", neg_parts.join(" "))
            })
        };
        for post in &spec_node.contract.post {
            let p_smt = encode_assume_depth(post, &mut s_env, qb, unfold_depth + 1);
            match &neg_pre {
                None => qb.assertions.push(p_smt),
                Some(np) => qb.assertions.push(format!("(or {np} {p_smt})")),
            }
        }
        // ADR 0008: 1-level body unfolding when unfold_depth == 0.
        if unfold_depth == 0
            && let Some(unfolded_results) = inline_pure_node(
                &spec_node,
                spec_hash,
                &arg_vals,
                &format!("spec_{hex12}"),
                env.callee,
                qb,
                1,
            )
        {
            let unfolded_val = if spec_node.results.len() == 1 {
                unfolded_results[0].clone()
            } else {
                SymVal::Tuple {
                    elems: spec_node
                        .results
                        .iter()
                        .cloned()
                        .zip(unfolded_results)
                        .collect(),
                }
            };
            if let Some(eq) =
                try_scalar_or_val_same(&res_ty, &res_sym, &unfolded_val, &spec_node, env.callee, qb)
            {
                match &neg_pre {
                    None => qb.assertions.push(eq),
                    Some(np) => qb.assertions.push(format!("(or {np} {eq})")),
                }
            }
        }
    }
    symval_to_fval(&res_sym, &res_ty)
}

fn eval_fold(
    call_ix: u32,
    args: &[Term],
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> FVal {
    let kernel_hash = env.node.calls[call_ix as usize];
    let Some(kernel) = (env.callee)(&kernel_hash) else {
        qb.unresolved_error.get_or_insert_with(|| {
            format!(
                "missing kernel {} for fold: call",
                vg_core::ir::hex(&kernel_hash)
            )
        });
        return FVal::Int {
            expr: bv_lit_i128(0, 65),
            width: 65,
            orig: None,
        };
    };
    let acc_ty = kernel.params[0].clone();
    let elem_ty = kernel.params[1].clone();

    let init_fv = eval_term_depth(&args[0], env, qb, unfold_depth);
    let init_sv = fval_to_symval(&init_fv, &acc_ty).expect("fold init type");
    let FVal::Val {
        val: xs_sv,
        ty: xs_ty,
    } = eval_term_depth(&args[1], env, qb, unfold_depth)
    else {
        panic!("fold xs must be collection");
    };
    let k_fv = eval_term_depth(&args[2], env, qb, unfold_depth);
    let (ke, kw) = k_fv.into_int().expect("fold k must be Int");
    let k65 = if kw >= 65 {
        extract_lo(&ke, kw, 65)
    } else {
        sext(&ke, kw, 65)
    };
    let mut b_vals = Vec::with_capacity(args.len().saturating_sub(3));
    for (ba, bt) in args[3..].iter().zip(&kernel.params[2..]) {
        let fv = eval_term_depth(ba, env, qb, unfold_depth);
        b_vals.push((bt.clone(), fval_to_symval(&fv, bt).expect("fold b arg")));
    }

    let xs_len_bv64 = match &xs_sv {
        SymVal::Array { len, .. } => bv_lit_u128(u128::from(*len), 64),
        SymVal::Slice { len, .. } => len.clone(),
        _ => panic!("fold xs must be Array or Slice"),
    };

    // Normalize `xs` into `(len_bv64, arr_0..)` so fold UF takes a canonical Slice signature.
    let xs_slice_ty = Type::Slice(Box::new(elem_ty.clone()));
    let xs_arrays = xs_sv.arrays().expect("xs arrays").to_vec();
    let xs_as_slice = SymVal::Slice {
        coll_id: xs_sv.coll_id().unwrap_or(0),
        len: xs_len_bv64.clone(),
        elem_ty: elem_ty.clone(),
        arrays: xs_arrays,
    };

    let mut flat_sorts = Vec::new();
    let mut flat_args = Vec::new();
    for (_, s) in flatten_type(&acc_ty) {
        flat_sorts.push(s.to_string());
    }
    flat_args.extend(symval_atoms(&init_sv));
    for (_, s) in flatten_type(&xs_slice_ty) {
        flat_sorts.push(s.to_string());
    }
    flat_args.extend(symval_atoms(&xs_as_slice));
    flat_sorts.push("(_ BitVec 65)".to_string());
    flat_args.push(k65.clone());
    for (bt, bv) in &b_vals {
        for (_, s) in flatten_type(bt) {
            flat_sorts.push(s.to_string());
        }
        flat_args.extend(symval_atoms(bv));
    }

    let hex12 = &vg_core::ir::hex(&kernel_hash)[..12];
    let make_fold_app = |k_expr: &str, qb_inner: &mut QueryBuilder| -> SymVal {
        let out_sorts = flatten_type(&acc_ty);
        let mut atoms = Vec::with_capacity(out_sorts.len());
        let mut args_with_k = Vec::new();
        args_with_k.extend(symval_atoms(&init_sv));
        args_with_k.extend(symval_atoms(&xs_as_slice));
        args_with_k.push(k_expr.to_string());
        for (_, bv) in &b_vals {
            args_with_k.extend(symval_atoms(bv));
        }
        for (c, (_, s)) in out_sorts.into_iter().enumerate() {
            let uf = format!("fold_{hex12}_{c}");
            qb_inner.declare_fun_once(&uf, flat_sorts.clone(), s.to_string());
            atoms.push(format!("({uf} {})", args_with_k.join(" ")));
        }
        let sv = symval_from_atoms(&acc_ty, &atoms, &mut qb_inner.next_coll_id);
        qb_inner.register_coll_id(&sv);
        sv
    };

    let res_sym = make_fold_app(&k65, qb);
    // ADR 0008: emit defining equations once per ground application without chaining further.
    let key = format!("{hex12}:{}", flat_args.join(","));
    if qb.fold_apps.insert(key) {
        let zero65 = bv_lit_i128(0, 65);
        let one65 = bv_lit_i128(1, 65);
        if let Some(eq_base) =
            try_scalar_or_val_same(&acc_ty, &res_sym, &init_sv, env.node, env.callee, qb)
        {
            qb.assertions
                .push(format!("(=> (= {k65} {zero65}) {eq_base})"));
        }

        let k_minus_1 = format!("(bvsub {k65} {one65})");
        let prev_acc = make_fold_app(&k_minus_1, qb);
        let idx_bv64 = extract_lo(&k_minus_1, 65, 64);
        let elem_sym = symval_select(&xs_sv, &idx_bv64, &mut qb.next_coll_id);
        qb.register_coll_id(&elem_sym);
        if let Some(cid) = xs_sv.coll_id() {
            qb.index_pool.entry(cid).or_default().insert(IndexAccess {
                dims: vec![idx_bv64.clone()],
                flat: idx_bv64,
            });
        }
        let _ = xs_ty;
        let mut step_args = Vec::with_capacity(2 + b_vals.len());
        step_args.push((acc_ty.clone(), prev_acc));
        step_args.push((elem_ty, elem_sym));
        step_args.extend(b_vals);
        if let Some(step_res) = inline_pure_node(
            &kernel,
            kernel_hash,
            &step_args,
            &format!("fld_{hex12}"),
            env.callee,
            qb,
            unfold_depth + 1,
        ) && let Some(step_out) = step_res.first()
            && let Some(eq_step) =
                try_scalar_or_val_same(&acc_ty, &res_sym, step_out, &kernel, env.callee, qb)
        {
            let len65 = zext(&xs_len_bv64, 64, 65);
            qb.assertions.push(format!(
                "(=> (and (bvsgt {k65} {zero65}) (bvsle {k65} {len65})) {eq_step})"
            ));
        }
    }
    symval_to_fval(&res_sym, &acc_ty)
}

fn eval_apply(
    call_ix: u32,
    args: &[Term],
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> FVal {
    let kernel_hash = env.node.calls[call_ix as usize];
    let Some(kernel) = (env.callee)(&kernel_hash) else {
        qb.unresolved_error.get_or_insert_with(|| {
            format!(
                "missing kernel {} for formula call",
                vg_core::ir::hex(&kernel_hash)
            )
        });
        return FVal::Bool("false".to_string());
    };
    let mut arg_vals = Vec::with_capacity(args.len());
    for (a, p_ty) in args.iter().zip(&kernel.params) {
        let fv = eval_term_depth(a, env, qb, unfold_depth);
        let sv = fval_to_symval(&fv, p_ty).expect("apply arg type");
        arg_vals.push((p_ty.clone(), sv));
    }
    let res_ty = if kernel.results.len() == 1 {
        kernel.results[0].clone()
    } else {
        Type::Tuple(kernel.results.clone())
    };
    if let Some(res_vec) = inline_pure_node(
        &kernel,
        kernel_hash,
        &arg_vals,
        "app",
        env.callee,
        qb,
        unfold_depth + 1,
    ) {
        let res_sv = if kernel.results.len() == 1 {
            res_vec[0].clone()
        } else {
            SymVal::Tuple {
                elems: kernel.results.iter().cloned().zip(res_vec).collect(),
            }
        };
        symval_to_fval(&res_sv, &res_ty)
    } else {
        let sv = qb.fresh_symval("app_err", &res_ty);
        symval_to_fval(&sv, &res_ty)
    }
}

fn kernel_contract_facts(
    kernel: &Node,
    arg_vals: &[(Type, SymVal)],
    out_val: &SymVal,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    qb: &mut QueryBuilder,
) -> (String, String) {
    let mut k_env = FormulaEnv {
        node: kernel,
        callee,
        inputs: arg_vals.to_vec(),
        results: vec![(kernel.results[0].clone(), out_val.clone())],
        ports: BTreeMap::new(),
        bound: Vec::new(),
        resources: None,
    };
    let pre = if kernel.contract.pre.is_empty() {
        "true".to_string()
    } else {
        let ps: Vec<String> = kernel
            .contract
            .pre
            .iter()
            .map(|p| encode_assume(p, &mut k_env, qb))
            .collect();
        if ps.len() == 1 {
            ps.into_iter().next().expect("1")
        } else {
            format!("(and {})", ps.join(" "))
        }
    };
    let post = if kernel.contract.post.is_empty() {
        String::new()
    } else {
        let qs: Vec<String> = kernel
            .contract
            .post
            .iter()
            .map(|q| encode_assume(q, &mut k_env, qb))
            .collect();
        if qs.len() == 1 {
            qs.into_iter().next().expect("1")
        } else {
            format!("(and {})", qs.join(" "))
        }
    };
    (pre, post)
}

fn try_scalar_or_val_same(
    ty: &Type,
    a: &SymVal,
    b: &SymVal,
    env_node: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    qb: &mut QueryBuilder,
) -> Option<String> {
    if let Some(s) = scalar_same_smt(ty, a, b) {
        return Some(s);
    }
    let mut dummy_env = FormulaEnv {
        node: env_node,
        callee,
        inputs: Vec::new(),
        results: Vec::new(),
        ports: BTreeMap::new(),
        bound: Vec::new(),
        resources: None,
    };
    Some(encode_val_eq_assume(ty, a, b, true, &mut dummy_env, qb))
}

/// Evaluates `Collection`/`Tuple`/`Result` equality in Assume polarity (exact up to len 16, else `PendingForall`).
pub fn encode_val_eq_assume(
    ty: &Type,
    a: &SymVal,
    b: &SymVal,
    is_same: bool,
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
) -> String {
    if !is_same
        && let (
            Type::Float(_) | Type::BFloat16,
            SymVal::Float { expr: fa, .. },
            SymVal::Float { expr: fb, .. },
        ) = (ty, a, b)
    {
        return format!("(fp.eq {fa} {fb})");
    }
    if let Some(s) = scalar_same_smt(ty, a, b)
        && (is_same || !contains_float(ty))
    {
        return s;
    }
    match (ty, a, b) {
        (Type::Tuple(ts), SymVal::Tuple { elems: ea }, SymVal::Tuple { elems: eb }) => {
            let parts: Vec<String> = ts
                .iter()
                .zip(ea)
                .zip(eb)
                .map(|((t, (_, va)), (_, vb))| encode_val_eq_assume(t, va, vb, is_same, env, qb))
                .collect();
            if parts.len() == 1 {
                parts.into_iter().next().expect("1")
            } else {
                format!("(and {})", parts.join(" "))
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
            let ok_eq = encode_val_eq_assume(ok_t, va_ok, vb_ok, is_same, env, qb);
            let err_eq = encode_val_eq_assume(err_t, va_err, vb_err, is_same, env, qb);
            format!("(and (= {oa} {ob}) (ite {oa} {ok_eq} {err_eq}))")
        }
        (Type::Array(elem_ty, len), _, _) => {
            if *len <= 16 {
                let mut parts = Vec::with_capacity(*len as usize);
                for idx in 0..*len {
                    let idx_lit = bv_lit_u128(u128::from(idx), 64);
                    let va = symval_select(a, &idx_lit, &mut qb.next_coll_id);
                    let vb = symval_select(b, &idx_lit, &mut qb.next_coll_id);
                    parts.push(encode_val_eq_assume(elem_ty, &va, &vb, is_same, env, qb));
                }
                if parts.is_empty() {
                    "true".to_string()
                } else if parts.len() == 1 {
                    parts.into_iter().next().expect("1")
                } else {
                    format!("(and {})", parts.join(" "))
                }
            } else {
                qb.pending_foralls.push(PendingForall::CollEq {
                    len_bv64: bv_lit_u128(u128::from(*len), 64),
                    left: a.clone(),
                    right: b.clone(),
                    elem_ty: (**elem_ty).clone(),
                    is_same,
                    env_node: env.node.clone(),
                });
                "true".to_string()
            }
        }
        (Type::Tensor(elem_ty, dims), _, _) => {
            let total: u64 = dims
                .iter()
                .fold(1u64, |acc, &d| acc.saturating_mul(u64::from(d)));
            if total <= 16 {
                let mut parts = Vec::with_capacity(total as usize);
                for idx in 0..total {
                    let idx_lit = bv_lit_u128(u128::from(idx), 64);
                    let va = symval_select(a, &idx_lit, &mut qb.next_coll_id);
                    let vb = symval_select(b, &idx_lit, &mut qb.next_coll_id);
                    parts.push(encode_val_eq_assume(elem_ty, &va, &vb, is_same, env, qb));
                }
                if parts.is_empty() {
                    "true".to_string()
                } else if parts.len() == 1 {
                    parts.into_iter().next().expect("1")
                } else {
                    format!("(and {})", parts.join(" "))
                }
            } else {
                qb.pending_foralls.push(PendingForall::CollEq {
                    len_bv64: bv_lit_u128(u128::from(total), 64),
                    left: a.clone(),
                    right: b.clone(),
                    elem_ty: (**elem_ty).clone(),
                    is_same,
                    env_node: env.node.clone(),
                });
                "true".to_string()
            }
        }
        (Type::Slice(elem_ty), SymVal::Slice { len: la, .. }, SymVal::Slice { len: lb, .. }) => {
            qb.pending_foralls.push(PendingForall::CollEq {
                len_bv64: la.clone(),
                left: a.clone(),
                right: b.clone(),
                elem_ty: (**elem_ty).clone(),
                is_same,
                env_node: env.node.clone(),
            });
            format!("(= {la} {lb})")
        }
        _ => scalar_same_smt(ty, a, b).unwrap_or_else(|| "true".to_string()),
    }
}

/// Negated equality `!(a == b)` in Goal polarity (Skolemizes element index for large collections/Slices).
// `env` keeps the signature of the other encoders; only the recursion reads it.
#[allow(clippy::only_used_in_recursion)]
pub fn encode_val_eq_goal_neg(
    ty: &Type,
    a: &SymVal,
    b: &SymVal,
    is_same: bool,
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
) -> String {
    if !is_same
        && let (
            Type::Float(_) | Type::BFloat16,
            SymVal::Float { expr: fa, .. },
            SymVal::Float { expr: fb, .. },
        ) = (ty, a, b)
    {
        return format!("(not (fp.eq {fa} {fb}))");
    }
    if let Some(s) = scalar_same_smt(ty, a, b)
        && (is_same || !contains_float(ty))
    {
        return format!("(not {s})");
    }
    match (ty, a, b) {
        (Type::Tuple(ts), SymVal::Tuple { elems: ea }, SymVal::Tuple { elems: eb }) => {
            let parts: Vec<String> = ts
                .iter()
                .zip(ea)
                .zip(eb)
                .map(|((t, (_, va)), (_, vb))| encode_val_eq_goal_neg(t, va, vb, is_same, env, qb))
                .collect();
            if parts.len() == 1 {
                parts.into_iter().next().expect("1")
            } else {
                format!("(or {})", parts.join(" "))
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
            let ok_neg = encode_val_eq_goal_neg(ok_t, va_ok, vb_ok, is_same, env, qb);
            let err_neg = encode_val_eq_goal_neg(err_t, va_err, vb_err, is_same, env, qb);
            format!("(or (not (= {oa} {ob})) (ite {oa} {ok_neg} {err_neg}))")
        }
        (Type::Array(elem_ty, len), _, _) => {
            if *len <= 16 {
                let mut parts = Vec::with_capacity(*len as usize);
                for idx in 0..*len {
                    let idx_lit = bv_lit_u128(u128::from(idx), 64);
                    let va = symval_select(a, &idx_lit, &mut qb.next_coll_id);
                    let vb = symval_select(b, &idx_lit, &mut qb.next_coll_id);
                    parts.push(encode_val_eq_goal_neg(elem_ty, &va, &vb, is_same, env, qb));
                }
                if parts.is_empty() {
                    "false".to_string()
                } else if parts.len() == 1 {
                    parts.into_iter().next().expect("1")
                } else {
                    format!("(or {})", parts.join(" "))
                }
            } else {
                let sk = qb.fresh_sym("eq_sk", "(_ BitVec 64)");
                qb.record_index(a, vec![sk.clone()], sk.clone());
                qb.record_index(b, vec![sk.clone()], sk.clone());
                let va = symval_select(a, &sk, &mut qb.next_coll_id);
                let vb = symval_select(b, &sk, &mut qb.next_coll_id);
                let elem_neg = encode_val_eq_goal_neg(elem_ty, &va, &vb, is_same, env, qb);
                let len_lit = bv_lit_u128(u128::from(*len), 64);
                format!("(and (bvult {sk} {len_lit}) {elem_neg})")
            }
        }
        (Type::Tensor(elem_ty, dims), _, _) => {
            let total: u64 = dims
                .iter()
                .fold(1u64, |acc, &d| acc.saturating_mul(u64::from(d)));
            if total <= 16 {
                let mut parts = Vec::with_capacity(total as usize);
                for idx in 0..total {
                    let idx_lit = bv_lit_u128(u128::from(idx), 64);
                    let va = symval_select(a, &idx_lit, &mut qb.next_coll_id);
                    let vb = symval_select(b, &idx_lit, &mut qb.next_coll_id);
                    parts.push(encode_val_eq_goal_neg(elem_ty, &va, &vb, is_same, env, qb));
                }
                if parts.is_empty() {
                    "false".to_string()
                } else if parts.len() == 1 {
                    parts.into_iter().next().expect("1")
                } else {
                    format!("(or {})", parts.join(" "))
                }
            } else {
                let sk = qb.fresh_sym("eq_sk", "(_ BitVec 64)");
                qb.record_index(a, vec![sk.clone()], sk.clone());
                qb.record_index(b, vec![sk.clone()], sk.clone());
                let va = symval_select(a, &sk, &mut qb.next_coll_id);
                let vb = symval_select(b, &sk, &mut qb.next_coll_id);
                let elem_neg = encode_val_eq_goal_neg(elem_ty, &va, &vb, is_same, env, qb);
                let len_lit = bv_lit_u128(u128::from(total), 64);
                format!("(and (bvult {sk} {len_lit}) {elem_neg})")
            }
        }
        (Type::Slice(elem_ty), SymVal::Slice { len: la, .. }, SymVal::Slice { len: lb, .. }) => {
            let sk = qb.fresh_sym("eq_sk", "(_ BitVec 64)");
            qb.record_index(a, vec![sk.clone()], sk.clone());
            qb.record_index(b, vec![sk.clone()], sk.clone());
            let va = symval_select(a, &sk, &mut qb.next_coll_id);
            let vb = symval_select(b, &sk, &mut qb.next_coll_id);
            let elem_neg = encode_val_eq_goal_neg(elem_ty, &va, &vb, is_same, env, qb);
            format!("(or (not (= {la} {lb})) (and (bvult {sk} {la}) {elem_neg}))")
        }
        _ => format!(
            "(not {})",
            scalar_same_smt(ty, a, b).unwrap_or_else(|| "true".into())
        ),
    }
}

fn contains_float(ty: &Type) -> bool {
    match ty {
        Type::Float(_) | Type::BFloat16 => true,
        Type::Array(e, _) | Type::Tensor(e, _) | Type::Slice(e) => contains_float(e),
        Type::Tuple(ms) => ms.iter().any(contains_float),
        Type::Result(a, b) => contains_float(a) || contains_float(b),
        _ => false,
    }
}

fn contains_forall(t: &Term) -> bool {
    match t {
        Term::Forall(..) => true,
        Term::Not(a) | Term::Neg(a) | Term::Proj(a, _) => contains_forall(a),
        Term::Bin(_, a, b) => contains_forall(a) || contains_forall(b),
        Term::Index(a, xs) => contains_forall(a) || xs.iter().any(contains_forall),
        Term::Builtin(_, xs) | Term::Spec(_, xs) | Term::Fold(_, xs) | Term::Apply(_, xs) => {
            xs.iter().any(contains_forall)
        }
        _ => false,
    }
}

fn expand_bool_eq(is_eq: bool, a: &Term, b: &Term) -> Term {
    let bx = Box::new;
    if is_eq {
        Term::Bin(
            Bin::And,
            bx(Term::Bin(Bin::Implies, bx(a.clone()), bx(b.clone()))),
            bx(Term::Bin(Bin::Implies, bx(b.clone()), bx(a.clone()))),
        )
    } else {
        Term::Bin(
            Bin::And,
            bx(Term::Bin(Bin::Or, bx(a.clone()), bx(b.clone()))),
            bx(Term::Bin(
                Bin::Or,
                bx(Term::Not(bx(a.clone()))),
                bx(Term::Not(bx(b.clone()))),
            )),
        )
    }
}

/// Encodes `term` in Assumption polarity (over-approximating `term` soundly).
pub fn encode_assume(term: &Term, env: &mut FormulaEnv<'_>, qb: &mut QueryBuilder) -> String {
    encode_assume_depth(term, env, qb, 0)
}

fn encode_assume_depth(
    term: &Term,
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> String {
    match term {
        Term::Not(a) => encode_neg_goal_depth(a, env, qb, unfold_depth),
        Term::Bin(Bin::And, a, b) => format!(
            "(and {} {})",
            encode_assume_depth(a, env, qb, unfold_depth),
            encode_assume_depth(b, env, qb, unfold_depth)
        ),
        Term::Bin(Bin::Or, a, b) => format!(
            "(or {} {})",
            encode_assume_depth(a, env, qb, unfold_depth),
            encode_assume_depth(b, env, qb, unfold_depth)
        ),
        Term::Bin(Bin::Implies, a, b) => format!(
            "(or {} {})",
            encode_neg_goal_depth(a, env, qb, unfold_depth),
            encode_assume_depth(b, env, qb, unfold_depth)
        ),
        Term::Bin(op @ (Bin::Eq | Bin::Ne), a, b) if contains_forall(a) || contains_forall(b) => {
            let expanded = expand_bool_eq(*op == Bin::Eq, a, b);
            encode_assume_depth(&expanded, env, qb, unfold_depth)
        }
        Term::Forall(ranges, body) => {
            let watches = collect_forall_watches(ranges.len(), body, env, qb, unfold_depth);
            qb.pending_foralls.push(PendingForall::Term {
                ranges: ranges.clone(),
                body: (**body).clone(),
                env: env.snapshot(),
                watches,
            });
            "true".to_string()
        }
        _ => eval_term_depth(term, env, qb, unfold_depth)
            .as_bool()
            .expect("boolean formula")
            .to_string(),
    }
}

/// Encodes `¬term` in Goal polarity (over-approximating `¬term` soundly by Skolemizing `Forall`).
pub fn encode_neg_goal(term: &Term, env: &mut FormulaEnv<'_>, qb: &mut QueryBuilder) -> String {
    encode_neg_goal_depth(term, env, qb, 0)
}

fn encode_neg_goal_depth(
    term: &Term,
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> String {
    match term {
        Term::Not(a) => encode_assume_depth(a, env, qb, unfold_depth),
        Term::Bin(Bin::And, a, b) => format!(
            "(or {} {})",
            encode_neg_goal_depth(a, env, qb, unfold_depth),
            encode_neg_goal_depth(b, env, qb, unfold_depth)
        ),
        Term::Bin(Bin::Or, a, b) => format!(
            "(and {} {})",
            encode_neg_goal_depth(a, env, qb, unfold_depth),
            encode_neg_goal_depth(b, env, qb, unfold_depth)
        ),
        Term::Bin(Bin::Implies, a, b) => format!(
            "(and {} {})",
            encode_assume_depth(a, env, qb, unfold_depth),
            encode_neg_goal_depth(b, env, qb, unfold_depth)
        ),
        Term::Bin(op @ (Bin::Eq | Bin::Ne), a, b) => {
            if contains_forall(a) || contains_forall(b) {
                let expanded = expand_bool_eq(*op == Bin::Eq, a, b);
                return encode_neg_goal_depth(&expanded, env, qb, unfold_depth);
            }
            let va = eval_term_depth(a, env, qb, unfold_depth);
            let vb = eval_term_depth(b, env, qb, unfold_depth);
            match (&va, &vb) {
                (FVal::Val { val: sa, ty }, FVal::Val { val: sb, .. }) => {
                    if *op == Bin::Eq {
                        encode_val_eq_goal_neg(ty, sa, sb, false, env, qb)
                    } else {
                        encode_val_eq_assume(ty, sa, sb, false, env, qb)
                    }
                }
                _ => {
                    let eq = match (&va, &vb) {
                        (FVal::Bool(x), FVal::Bool(y)) => format!("(= {x} {y})"),
                        (FVal::Int { .. }, FVal::Int { .. }) => eval_int_cmp(Bin::Eq, va, vb),
                        (FVal::Float { expr: x, .. }, FVal::Float { expr: y, .. }) => {
                            format!("(fp.eq {x} {y})")
                        }
                        _ => panic!("mismatched == operands"),
                    };
                    if *op == Bin::Eq {
                        format!("(not {eq})")
                    } else {
                        eq
                    }
                }
            }
        }
        Term::Builtin(Builtin::Same, args) => {
            let va = eval_term_depth(&args[0], env, qb, unfold_depth);
            let vb = eval_term_depth(&args[1], env, qb, unfold_depth);
            match (&va, &vb) {
                (FVal::Val { val: sa, ty }, FVal::Val { val: sb, .. }) => {
                    encode_val_eq_goal_neg(ty, sa, sb, true, env, qb)
                }
                _ => {
                    let s = match (&va, &vb) {
                        (FVal::Bool(a), FVal::Bool(b)) => format!("(= {a} {b})"),
                        (FVal::Int { .. }, FVal::Int { .. }) => eval_int_cmp(Bin::Eq, va, vb),
                        (FVal::Float { expr: fa, .. }, FVal::Float { expr: fb, .. }) => format!(
                            "(or (and (fp.isNaN {fa}) (fp.isNaN {fb})) \
                             (and (fp.eq {fa} {fb}) (= (fp.isNegative {fa}) (fp.isNegative {fb}))))"
                        ),
                        _ => panic!("same type mismatch"),
                    };
                    format!("(not {s})")
                }
            }
        }
        Term::Forall(ranges, body) => {
            let bound0 = env.bound.len();
            let mut range_conds = Vec::with_capacity(ranges.len());
            for (lo, hi) in ranges {
                let (le, lw) = eval_term_depth(lo, env, qb, unfold_depth)
                    .into_int()
                    .expect("int range lo");
                let (he, hw) = eval_term_depth(hi, env, qb, unfold_depth)
                    .into_int()
                    .expect("int range hi");
                let w = lw.max(hw);
                let sk = qb.fresh_sym("sk", &format!("(_ BitVec {w})"));
                let ls = sext(&le, lw, w);
                let hs = sext(&he, hw, w);
                range_conds.push(format!("(and (bvsle {ls} {sk}) (bvslt {sk} {hs}))"));
                env.bound.push(FVal::Int {
                    expr: sk,
                    width: w,
                    orig: None,
                });
            }
            let neg_body = encode_neg_goal_depth(body, env, qb, unfold_depth);
            env.bound.truncate(bound0);
            format!("(and {} {neg_body})", range_conds.join(" "))
        }
        _ => {
            let b = eval_term_depth(term, env, qb, unfold_depth)
                .as_bool()
                .expect("boolean formula")
                .to_string();
            format!("(not {b})")
        }
    }
}

fn collect_forall_watches(
    num_ranges: usize,
    body: &Term,
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> Vec<ForallWatch> {
    let mut watches = Vec::new();
    walk_watches(num_ranges, 0, body, env, qb, unfold_depth, &mut watches);
    watches
}

fn walk_watches(
    num_ranges: usize,
    extra_depth: usize,
    t: &Term,
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
    out: &mut Vec<ForallWatch>,
) {
    match t {
        Term::Index(base, idxs) => {
            if !mentions_bound_range(base, num_ranges + extra_depth, extra_depth)
                && let FVal::Val { val, .. } = eval_term_depth(base, env, qb, unfold_depth)
                && let Some(cid) = val.coll_id()
            {
                let mut dim_patterns = Vec::new();
                for ix in idxs {
                    if let Some(p) = match_idx_pattern(ix, num_ranges, extra_depth, env.node) {
                        dim_patterns.push(p);
                    }
                }
                if dim_patterns.len() == idxs.len() {
                    out.push(ForallWatch {
                        coll_id: cid,
                        dim_patterns,
                    });
                }
            }
            walk_watches(num_ranges, extra_depth, base, env, qb, unfold_depth, out);
            for i in idxs {
                walk_watches(num_ranges, extra_depth, i, env, qb, unfold_depth, out);
            }
        }
        Term::Forall(rs, b) => {
            for (l, h) in rs {
                walk_watches(num_ranges, extra_depth, l, env, qb, unfold_depth, out);
                walk_watches(num_ranges, extra_depth, h, env, qb, unfold_depth, out);
            }
            walk_watches(
                num_ranges,
                extra_depth + rs.len(),
                b,
                env,
                qb,
                unfold_depth,
                out,
            );
        }
        _ => {
            for c in vg_core::canon::children(t) {
                walk_watches(num_ranges, extra_depth, c, env, qb, unfold_depth, out);
            }
        }
    }
}

fn mentions_bound_range(t: &Term, total_bound: usize, extra: usize) -> bool {
    match t {
        Term::Bound(d) => (*d as usize) >= extra && (*d as usize) < total_bound,
        Term::Forall(rs, b) => {
            rs.iter().any(|(l, h)| {
                mentions_bound_range(l, total_bound, extra)
                    || mentions_bound_range(h, total_bound, extra)
            }) || mentions_bound_range(b, total_bound + rs.len(), extra + rs.len())
        }
        _ => vg_core::canon::children(t)
            .into_iter()
            .any(|c| mentions_bound_range(c, total_bound, extra)),
    }
}

fn match_idx_pattern(t: &Term, num_ranges: usize, extra: usize, node: &Node) -> Option<IdxPattern> {
    let var_of = |d: u32| -> Option<usize> {
        let d = d as usize;
        if d >= extra && d < extra + num_ranges {
            Some(extra + num_ranges - 1 - d)
        } else {
            None
        }
    };
    match t {
        Term::Bound(d) => var_of(*d).map(IdxPattern::Var),
        Term::Bin(Bin::Add, a, b) => {
            if let Term::Bound(d) = **a
                && let Some(p) = var_of(d)
                && let Some(c) = eval_const_int(b, node)
            {
                return Some(IdxPattern::VarPlus(p, c));
            }
            if let Term::Bound(d) = **b
                && let Some(p) = var_of(d)
                && let Some(c) = eval_const_int(a, node)
            {
                return Some(IdxPattern::VarPlus(p, c));
            }
            None
        }
        Term::Bin(Bin::Sub, a, b) => {
            if let Term::Bound(d) = **a
                && let Some(p) = var_of(d)
                && let Some(c) = eval_const_int(b, node)
            {
                return Some(IdxPattern::VarPlus(p, -c));
            }
            if let Term::Bound(d) = **b
                && let Some(p) = var_of(d)
                && let Some(c) = eval_const_int(a, node)
            {
                return Some(IdxPattern::ConstMinusVar(c, p));
            }
            None
        }
        _ => None,
    }
}

/// Typed condition for ADR 0010 well-definedness checks (avoids mutating `node.consts`).
#[derive(Clone, Debug)]
// One per obligation, built once; boxing the large variant would only add indirection.
#[allow(clippy::large_enum_variant)]
pub enum WdCond {
    DivisorNonZero(Term),
    IndexInBounds {
        base: Term,
        idxs: Vec<Term>,
    },
    ResultIsOk(Term),
    ResultIsErr(Term),
    FoldBound {
        xs: Term,
        k: Term,
    },
    CalleePre {
        callee_node: Node,
        args: Vec<Term>,
        pre: Term,
    },
    IntArgFitsType {
        arg: Term,
        target: Type,
    },
    MissingCallee(Hash),
}

#[derive(Clone, Debug)]
pub struct WdObligation {
    pub path_hyps: Vec<Term>,
    /// `(lo, hi)` binders active at this point, outermost first.
    pub bound_ranges: Vec<(Term, Term)>,
    pub cond: WdCond,
    pub reason: String,
}

pub fn collect_wd_obligations(
    term: &Term,
    node: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Vec<WdObligation> {
    let mut out = Vec::new();
    let mut hyps = Vec::new();
    let mut ranges = Vec::new();
    walk_wd(term, node, callee, &mut hyps, &mut ranges, &mut out);
    out
}

fn walk_wd(
    t: &Term,
    node: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    hyps: &mut Vec<Term>,
    ranges: &mut Vec<(Term, Term)>,
    out: &mut Vec<WdObligation>,
) {
    let snap =
        |cond: WdCond, reason: String, hyps: &[Term], ranges: &[(Term, Term)]| WdObligation {
            path_hyps: hyps.to_vec(),
            bound_ranges: ranges.to_vec(),
            cond,
            reason,
        };
    match t {
        Term::Bin(Bin::And, a, b) => {
            walk_wd(a, node, callee, hyps, ranges, out);
            hyps.push((**a).clone());
            walk_wd(b, node, callee, hyps, ranges, out);
            hyps.pop();
        }
        Term::Bin(Bin::Or, a, b) => {
            walk_wd(a, node, callee, hyps, ranges, out);
            hyps.push(Term::Not(Box::new((**a).clone())));
            walk_wd(b, node, callee, hyps, ranges, out);
            hyps.pop();
        }
        Term::Bin(Bin::Implies, a, b) => {
            walk_wd(a, node, callee, hyps, ranges, out);
            hyps.push((**a).clone());
            walk_wd(b, node, callee, hyps, ranges, out);
            hyps.pop();
        }
        Term::Bin(Bin::Div | Bin::Mod, a, b) => {
            // Only integer division/mod has an unspecified zero divisor (float div gives +-inf/NaN).
            if is_int_term(t, node, callee, ranges.len()) {
                out.push(snap(
                    WdCond::DivisorNonZero((**b).clone()),
                    "divisor in formula must be non-zero".to_string(),
                    hyps,
                    ranges,
                ));
            }
            walk_wd(a, node, callee, hyps, ranges, out);
            walk_wd(b, node, callee, hyps, ranges, out);
        }
        Term::Index(base, idxs) => {
            out.push(snap(
                WdCond::IndexInBounds {
                    base: (**base).clone(),
                    idxs: idxs.clone(),
                },
                "formula index must be within bounds".to_string(),
                hyps,
                ranges,
            ));
            walk_wd(base, node, callee, hyps, ranges, out);
            for ix in idxs {
                walk_wd(ix, node, callee, hyps, ranges, out);
            }
        }
        Term::Builtin(Builtin::Ok, args) => {
            out.push(snap(
                WdCond::ResultIsOk(args[0].clone()),
                "ok(r) requires is_ok(r)".to_string(),
                hyps,
                ranges,
            ));
            walk_wd(&args[0], node, callee, hyps, ranges, out);
        }
        Term::Builtin(Builtin::Err, args) => {
            out.push(snap(
                WdCond::ResultIsErr(args[0].clone()),
                "err(r) requires !is_ok(r)".to_string(),
                hyps,
                ranges,
            ));
            walk_wd(&args[0], node, callee, hyps, ranges, out);
        }
        Term::Spec(c, args) | Term::Apply(c, args) => {
            let h = node.calls[*c as usize];
            if let Some(target) = callee(&h) {
                for (arg, pty) in args.iter().zip(&target.params) {
                    if pty.is_int() && !is_exact_int_of(arg, pty, node) {
                        out.push(snap(
                            WdCond::IntArgFitsType {
                                arg: arg.clone(),
                                target: pty.clone(),
                            },
                            format!("integer argument must fit parameter type {pty}"),
                            hyps,
                            ranges,
                        ));
                    }
                }
                for pre in &target.contract.pre {
                    out.push(snap(
                        WdCond::CalleePre {
                            callee_node: target.clone(),
                            args: args.clone(),
                            pre: pre.clone(),
                        },
                        "formula call arguments must satisfy callee PRE".to_string(),
                        hyps,
                        ranges,
                    ));
                }
            } else {
                out.push(snap(
                    WdCond::MissingCallee(h),
                    "missing callee node for formula call".to_string(),
                    hyps,
                    ranges,
                ));
            }
            for a in args {
                walk_wd(a, node, callee, hyps, ranges, out);
            }
        }
        Term::Fold(c, args) => {
            let h = node.calls[*c as usize];
            out.push(snap(
                WdCond::FoldBound {
                    xs: args[1].clone(),
                    k: args[2].clone(),
                },
                "fold: step count k must satisfy 0 <= k <= len(xs)".to_string(),
                hyps,
                ranges,
            ));
            if let Some(kernel) = callee(&h) {
                if kernel.params[0].is_int() && !is_exact_int_of(&args[0], &kernel.params[0], node)
                {
                    out.push(snap(
                        WdCond::IntArgFitsType {
                            arg: args[0].clone(),
                            target: kernel.params[0].clone(),
                        },
                        format!("fold: init must fit {}", kernel.params[0]),
                        hyps,
                        ranges,
                    ));
                }
                for (arg, pty) in args[3..].iter().zip(&kernel.params[2..]) {
                    if pty.is_int() && !is_exact_int_of(arg, pty, node) {
                        out.push(snap(
                            WdCond::IntArgFitsType {
                                arg: arg.clone(),
                                target: pty.clone(),
                            },
                            format!("fold: broadcast argument must fit {pty}"),
                            hyps,
                            ranges,
                        ));
                    }
                }
            } else {
                out.push(snap(
                    WdCond::MissingCallee(h),
                    "missing kernel node for fold:".to_string(),
                    hyps,
                    ranges,
                ));
            }
            for a in args {
                walk_wd(a, node, callee, hyps, ranges, out);
            }
        }
        Term::Forall(rs, body) => {
            let r0 = ranges.len();
            let h0 = hyps.len();
            for (lo, hi) in rs {
                walk_wd(lo, node, callee, hyps, ranges, out);
                walk_wd(hi, node, callee, hyps, ranges, out);
                for h in hyps.iter_mut() {
                    *h = shift_bound(h, 1, 0);
                }
                ranges.push((lo.clone(), hi.clone()));
            }
            walk_wd(body, node, callee, hyps, ranges, out);
            let added = ranges.len() - r0;
            ranges.truncate(r0);
            hyps.truncate(h0);
            if added > 0 {
                for h in hyps.iter_mut() {
                    *h = shift_bound(h, -(added as i32), 0);
                }
            }
        }
        _ => {
            for child in vg_core::canon::children(t) {
                walk_wd(child, node, callee, hyps, ranges, out);
            }
        }
    }
}

fn shift_bound(t: &Term, delta: i32, cutoff: u32) -> Term {
    let bx = Box::new;
    match t {
        Term::Bound(d) if *d >= cutoff => Term::Bound((*d as i32 + delta) as u32),
        Term::Not(a) => Term::Not(bx(shift_bound(a, delta, cutoff))),
        Term::Neg(a) => Term::Neg(bx(shift_bound(a, delta, cutoff))),
        Term::Bin(op, a, b) => Term::Bin(
            *op,
            bx(shift_bound(a, delta, cutoff)),
            bx(shift_bound(b, delta, cutoff)),
        ),
        Term::Index(a, xs) => Term::Index(
            bx(shift_bound(a, delta, cutoff)),
            xs.iter().map(|x| shift_bound(x, delta, cutoff)).collect(),
        ),
        Term::Proj(a, j) => Term::Proj(bx(shift_bound(a, delta, cutoff)), *j),
        Term::Forall(rs, b) => {
            let mut cur = cutoff;
            let mut new_rs = Vec::with_capacity(rs.len());
            for (l, h) in rs {
                new_rs.push((shift_bound(l, delta, cur), shift_bound(h, delta, cur)));
                cur += 1;
            }
            Term::Forall(new_rs, bx(shift_bound(b, delta, cur)))
        }
        Term::Builtin(b, xs) => Term::Builtin(
            *b,
            xs.iter().map(|x| shift_bound(x, delta, cutoff)).collect(),
        ),
        Term::Spec(c, xs) => Term::Spec(
            *c,
            xs.iter().map(|x| shift_bound(x, delta, cutoff)).collect(),
        ),
        Term::Fold(c, xs) => Term::Fold(
            *c,
            xs.iter().map(|x| shift_bound(x, delta, cutoff)).collect(),
        ),
        Term::Apply(c, xs) => Term::Apply(
            *c,
            xs.iter().map(|x| shift_bound(x, delta, cutoff)).collect(),
        ),
        other => other.clone(),
    }
}

fn is_exact_int_of(t: &Term, target: &Type, node: &Node) -> bool {
    match t {
        Term::Input(i) => node.input_type(*i) == Some(target),
        Term::Result(j) => node.results.get(*j as usize) == Some(target),
        Term::Port(i, s) => {
            node.ops
                .get(*i as usize)
                .and_then(|op| op.results.get(*s as usize))
                == Some(target)
        }
        Term::Const(c) => node
            .consts
            .get(*c as usize)
            .is_some_and(|c| c.ty == *target),
        _ => false,
    }
}

fn is_int_term(
    t: &Term,
    node: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    _depth: usize,
) -> bool {
    match t {
        Term::Input(i) => node.input_type(*i).is_some_and(Type::is_int),
        Term::Result(j) => node.results.get(*j as usize).is_some_and(Type::is_int),
        Term::Port(i, s) => node
            .ops
            .get(*i as usize)
            .and_then(|op| op.results.get(*s as usize))
            .is_some_and(Type::is_int),
        Term::Bound(_) | Term::Resource(_) => true,
        Term::Const(c) => {
            let ty = &node.consts[*c as usize].ty;
            *ty == Type::Int || ty.is_int()
        }
        Term::Neg(a) => is_int_term(a, node, callee, _depth),
        Term::Bin(Bin::Add | Bin::Sub | Bin::Mul | Bin::Div | Bin::Mod, a, b) => {
            is_int_term(a, node, callee, _depth) && is_int_term(b, node, callee, _depth)
        }
        Term::Builtin(Builtin::Len, _) => true,
        Term::Builtin(Builtin::Abs | Builtin::Min | Builtin::Max, args) => {
            is_int_term(&args[0], node, callee, _depth)
        }
        Term::Spec(c, _) | Term::Fold(c, _) | Term::Apply(c, _) => callee(&node.calls[*c as usize])
            .and_then(|n| n.results.first().cloned())
            .is_some_and(|r| r.is_int()),
        _ => false,
    }
}

/// Evaluates a `WdObligation` in the given `env` and returns the negated goal assertion string.
pub fn encode_wd_obligation(
    wd: &WdObligation,
    env: &mut FormulaEnv<'_>,
    qb: &mut QueryBuilder,
) -> String {
    let bound0 = env.bound.len();
    let mut range_assumes = Vec::new();
    for (lo, hi) in &wd.bound_ranges {
        let (le, lw) = eval_term(lo, env, qb).into_int().expect("wd range lo");
        let (he, hw) = eval_term(hi, env, qb).into_int().expect("wd range hi");
        let w = lw.max(hw);
        let sk = qb.fresh_sym("wd_k", &format!("(_ BitVec {w})"));
        let ls = sext(&le, lw, w);
        let hs = sext(&he, hw, w);
        range_assumes.push(format!("(and (bvsle {ls} {sk}) (bvslt {sk} {hs}))"));
        env.bound.push(FVal::Int {
            expr: sk,
            width: w,
            orig: None,
        });
    }
    let hyp_assumes: Vec<String> = wd
        .path_hyps
        .iter()
        .map(|h| encode_assume(h, env, qb))
        .collect();
    let neg_goal = match &wd.cond {
        WdCond::DivisorNonZero(b) => {
            let (be, bw) = eval_term(b, env, qb).into_int().expect("divisor int");
            format!("(= {be} {})", bv_lit_u128(0, bw))
        }
        WdCond::IndexInBounds { base, idxs } => {
            let FVal::Val { val, ty } = eval_term(base, env, qb) else {
                panic!("index base");
            };
            let idx_ints: Vec<(String, u32)> = idxs
                .iter()
                .map(|i| eval_term(i, env, qb).into_int().expect("index int"))
                .collect();
            match ty {
                Type::Array(_, len) => {
                    let (ie, iw) = &idx_ints[0];
                    let cw = (*iw).max(65);
                    let ix = sext(ie, *iw, cw);
                    format!(
                        "(not (and (bvsge {ix} {}) (bvslt {ix} {})))",
                        bv_lit_i128(0, cw),
                        bv_lit_i128(i128::from(len), cw)
                    )
                }
                Type::Slice(_) => {
                    let SymVal::Slice { len, .. } = val else {
                        unreachable!()
                    };
                    let (ie, iw) = &idx_ints[0];
                    let cw = (*iw).max(65);
                    let ix = sext(ie, *iw, cw);
                    let lx = zext(&len, 64, cw);
                    format!(
                        "(not (and (bvsge {ix} {}) (bvslt {ix} {lx})))",
                        bv_lit_i128(0, cw)
                    )
                }
                Type::Tensor(_, dims) => {
                    let mut parts = Vec::with_capacity(dims.len());
                    for (&(ref ie, iw), &d) in idx_ints.iter().zip(&dims) {
                        let cw = iw.max(65);
                        let ix = sext(ie, iw, cw);
                        parts.push(format!(
                            "(and (bvsge {ix} {}) (bvslt {ix} {}))",
                            bv_lit_i128(0, cw),
                            bv_lit_i128(i128::from(d), cw)
                        ));
                    }
                    format!("(not (and {}))", parts.join(" "))
                }
                _ => "true".to_string(),
            }
        }
        WdCond::ResultIsOk(r) => {
            let FVal::Val {
                val: SymVal::Result { is_ok, .. },
                ..
            } = eval_term(r, env, qb)
            else {
                panic!("Result");
            };
            format!("(not {is_ok})")
        }
        WdCond::ResultIsErr(r) => {
            let FVal::Val {
                val: SymVal::Result { is_ok, .. },
                ..
            } = eval_term(r, env, qb)
            else {
                panic!("Result");
            };
            is_ok
        }
        WdCond::FoldBound { xs, k } => {
            let FVal::Val { val, .. } = eval_term(xs, env, qb) else {
                panic!("fold xs");
            };
            let len65 = match val {
                SymVal::Array { len, .. } => bv_lit_i128(i128::from(len), 65),
                SymVal::Slice { len, .. } => zext(&len, 64, 65),
                _ => panic!("fold xs type"),
            };
            let (ke, kw) = eval_term(k, env, qb).into_int().expect("fold k int");
            let cw = kw.max(66);
            let ks = sext(&ke, kw, cw);
            let ls = sext(&len65, 65, cw);
            format!(
                "(not (and (bvsge {ks} {}) (bvsle {ks} {ls})))",
                bv_lit_i128(0, cw)
            )
        }
        WdCond::CalleePre {
            callee_node,
            args,
            pre,
        } => {
            let mut arg_vals = Vec::with_capacity(args.len());
            for (a, p_ty) in args.iter().zip(&callee_node.params) {
                let fv = eval_term(a, env, qb);
                let sv = fval_to_symval(&fv, p_ty).expect("callee arg type");
                arg_vals.push((p_ty.clone(), sv));
            }
            let mut c_env = FormulaEnv {
                node: callee_node,
                callee: env.callee,
                inputs: arg_vals,
                results: Vec::new(),
                ports: BTreeMap::new(),
                bound: Vec::new(),
                resources: None,
            };
            encode_neg_goal(pre, &mut c_env, qb)
        }
        WdCond::IntArgFitsType { arg, target } => {
            let (ae, aw) = eval_term(arg, env, qb).into_int().expect("int arg");
            let (lo, hi) = int_type_bounds(target);
            let cw = aw.max(66);
            let as_ = sext(&ae, aw, cw);
            format!(
                "(not (and (bvsge {as_} {}) (bvsle {as_} {})))",
                bv_lit_i128(lo, cw),
                bv_lit_i128(hi, cw)
            )
        }
        WdCond::MissingCallee(h) => {
            qb.unresolved_error.get_or_insert_with(|| {
                format!("missing callee {} in WD check", vg_core::ir::hex(h))
            });
            "true".to_string()
        }
    };
    env.bound.truncate(bound0);

    let mut all = Vec::new();
    all.extend(range_assumes);
    all.extend(hyp_assumes);
    all.push(neg_goal);
    if all.len() == 1 {
        all.into_iter().next().expect("1")
    } else {
        format!("(and {})", all.join(" "))
    }
}

pub fn int_type_bounds(ty: &Type) -> (i128, i128) {
    match ty {
        Type::UInt(w) => (0, (1i128 << *w) - 1),
        Type::SInt(w) => (-(1i128 << (*w - 1)), (1i128 << (*w - 1)) - 1),
        _ => panic!("not an int type: {ty}"),
    }
}

/// How `eval_op_core` handles partial operations outside their domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainMode {
    /// Used when encoding a verified node's own ops (obligations are checked separately).
    Strict,
    /// Used when inlining pure kernels or unfolding `SPEC` bodies inside formulas (docs/05):
    /// an out-of-domain op returns an unspecified uninterpreted value of the operands.
    PartialGuarded,
}

/// Evaluates one `Op` into its result `SymVal`s and optional extra domain/contract assertions.
#[allow(clippy::too_many_arguments)]
pub fn eval_op_core(
    node: &Node,
    op: &Op,
    arg_syms: &[SymVal],
    prefix: &str,
    mode: DomainMode,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> Vec<SymVal> {
    let info = opcode::by_code(op.opcode).expect("valid opcode");
    let single = |v: SymVal| vec![v];

    // Helper: wrap an op result in `(ite domain_ok normal (undef_uf operand_atoms...))` under PartialGuarded.
    let guard_partial = |qb_in: &mut QueryBuilder,
                         domain_ok: &str,
                         normal: SymVal,
                         res_ty: &Type,
                         args: &[SymVal],
                         tag: &str|
     -> SymVal {
        if mode == DomainMode::Strict {
            // The verify driver turns each recorded domain into the op's own obligation.
            qb_in.domains.push((tag.to_string(), domain_ok.to_string()));
            return normal;
        }
        let mut op_sorts = Vec::new();
        let mut op_atoms = Vec::new();
        for (a_op, a_sv) in op.operands.iter().zip(args) {
            if let Some(a_ty) = node.operand_type(*a_op) {
                for (_, s) in flatten_type(a_ty) {
                    op_sorts.push(s.to_string());
                }
                op_atoms.extend(symval_atoms(a_sv));
            }
        }
        let out_sorts = flatten_type(res_ty);
        let mut undef_atoms = Vec::with_capacity(out_sorts.len());
        for (k, (_, s)) in out_sorts.into_iter().enumerate() {
            let uf = format!("undef_{tag}_{}_{k}", ty_short_tag(res_ty));
            qb_in.declare_fun_once(&uf, op_sorts.clone(), s.to_string());
            if op_atoms.is_empty() {
                undef_atoms.push(uf);
            } else {
                undef_atoms.push(format!("({uf} {})", op_atoms.join(" ")));
            }
        }
        let undef = symval_from_atoms(res_ty, &undef_atoms, &mut qb_in.next_coll_id);
        let res = symval_ite(domain_ok, res_ty, &normal, &undef, &mut qb_in.next_coll_id);
        qb_in.register_coll_id(&res);
        res
    };

    match info.name {
        "IDENTITY" => single(arg_syms[0].clone()),
        "TUPLE" => {
            let Type::Tuple(ts) = &op.results[0] else {
                unreachable!()
            };
            single(SymVal::Tuple {
                elems: ts.iter().cloned().zip(arg_syms.iter().cloned()).collect(),
            })
        }
        "PROJ" => {
            let SymVal::Tuple { elems } = &arg_syms[0] else {
                unreachable!()
            };
            let Operand::Const(c) = op.operands[1] else {
                unreachable!()
            };
            let bytes = &node.consts[c as usize].bytes;
            let j = u32::from_le_bytes(bytes[..4].try_into().expect("u32")) as usize;
            single(elems[j].1.clone())
        }
        "SELECT" => {
            let SymVal::Bool(c) = &arg_syms[0] else {
                unreachable!()
            };
            let res_ty = &op.results[0];
            let res = symval_ite(c, res_ty, &arg_syms[1], &arg_syms[2], &mut qb.next_coll_id);
            qb.register_coll_id(&res);
            if let (Some(dst), Some(then_id), Some(else_id)) =
                (res.coll_id(), arg_syms[1].coll_id(), arg_syms[2].coll_id())
            {
                qb.coll_links.push(CollLink::Ite {
                    dst,
                    then_id,
                    else_id,
                });
            }
            single(res)
        }
        "CAST" => {
            let src_ty = node.operand_type(op.operands[0]).expect("cast src type");
            let dst_ty = &op.results[0];
            let (normal, domain) = encode_cast(src_ty, dst_ty, &arg_syms[0]);
            let res = match domain {
                Some(dom) => guard_partial(qb, &dom, normal, dst_ty, arg_syms, "cast"),
                None => normal,
            };
            single(res)
        }
        "AS_BYTES" => {
            let src_ty = node.operand_type(op.operands[0]).expect("as_bytes src");
            single(encode_as_bytes(src_ty, &arg_syms[0], qb))
        }
        "OK" => {
            let Type::Result(ok_ty, err_ty) = &op.results[0] else {
                unreachable!()
            };
            let dummy_err = qb.fresh_symval(&format!("{prefix}_ok_err"), err_ty);
            single(SymVal::Result {
                is_ok: "true".to_string(),
                ok_ty: (**ok_ty).clone(),
                ok: Box::new(arg_syms[0].clone()),
                err_ty: (**err_ty).clone(),
                err: Box::new(dummy_err),
            })
        }
        "ERR" => {
            let Type::Result(ok_ty, err_ty) = &op.results[0] else {
                unreachable!()
            };
            let dummy_ok = qb.fresh_symval(&format!("{prefix}_err_ok"), ok_ty);
            single(SymVal::Result {
                is_ok: "false".to_string(),
                ok_ty: (**ok_ty).clone(),
                ok: Box::new(dummy_ok),
                err_ty: (**err_ty).clone(),
                err: Box::new(arg_syms[0].clone()),
            })
        }
        "IS_OK" => {
            let SymVal::Result { is_ok, .. } = &arg_syms[0] else {
                unreachable!()
            };
            single(SymVal::Bool(is_ok.clone()))
        }
        "GET_OK" => {
            let SymVal::Result { is_ok, ok, .. } = &arg_syms[0] else {
                unreachable!()
            };
            let res = guard_partial(
                qb,
                is_ok,
                (**ok).clone(),
                &op.results[0],
                arg_syms,
                "get_ok",
            );
            single(res)
        }
        "GET_ERR" => {
            let SymVal::Result { is_ok, err, .. } = &arg_syms[0] else {
                unreachable!()
            };
            let cond = format!("(not {is_ok})");
            let res = guard_partial(
                qb,
                &cond,
                (**err).clone(),
                &op.results[0],
                arg_syms,
                "get_err",
            );
            single(res)
        }
        "ADD" | "SUB" | "MUL" => {
            let ty = &op.results[0];
            let SymVal::Int { expr: a, .. } = &arg_syms[0] else {
                unreachable!()
            };
            let SymVal::Int { expr: b, .. } = &arg_syms[1] else {
                unreachable!()
            };
            let (normal, domain) = encode_add_sub_mul(info.name, op.mode, ty, a, b);
            let res = match domain {
                Some(dom) => guard_partial(qb, &dom, normal, ty, arg_syms, info.name),
                None => normal,
            };
            single(res)
        }
        "DIV" | "MOD" => {
            let ty = &op.results[0];
            let SymVal::Int { expr: a, .. } = &arg_syms[0] else {
                unreachable!()
            };
            let SymVal::Int { expr: b, .. } = &arg_syms[1] else {
                unreachable!()
            };
            let (normal, domain) = encode_div_mod(info.name, op.mode, ty, a, b);
            let res = guard_partial(qb, &domain, normal, ty, arg_syms, info.name);
            single(res)
        }
        "NEG" => {
            let ty = &op.results[0];
            let SymVal::Int { expr: a, .. } = &arg_syms[0] else {
                unreachable!()
            };
            let (normal, domain) = encode_neg(op.mode, ty, a);
            let res = match domain {
                Some(dom) => guard_partial(qb, &dom, normal, ty, arg_syms, "NEG"),
                None => normal,
            };
            single(res)
        }
        "AND" | "OR" | "XOR" => {
            let ty = &op.results[0];
            let SymVal::Int { expr: a, .. } = &arg_syms[0] else {
                unreachable!()
            };
            let SymVal::Int { expr: b, .. } = &arg_syms[1] else {
                unreachable!()
            };
            let bvop = match info.name {
                "AND" => "bvand",
                "OR" => "bvor",
                "XOR" => "bvxor",
                _ => unreachable!(),
            };
            single(SymVal::Int {
                expr: format!("({bvop} {a} {b})"),
                ty: ty.clone(),
            })
        }
        "NOT" => {
            let ty = &op.results[0];
            let SymVal::Int { expr: a, .. } = &arg_syms[0] else {
                unreachable!()
            };
            single(SymVal::Int {
                expr: format!("(bvnot {a})"),
                ty: ty.clone(),
            })
        }
        "SHL" | "SHR" | "ROTL" | "ROTR" => {
            let ty = &op.results[0];
            let SymVal::Int { expr: a, .. } = &arg_syms[0] else {
                unreachable!()
            };
            let SymVal::Int { expr: s, .. } = &arg_syms[1] else {
                unreachable!()
            };
            let (normal, domain) = encode_shift_rotate(info.name, op.mode, ty, a, s);
            let res = match domain {
                Some(dom) => guard_partial(qb, &dom, normal, ty, arg_syms, info.name),
                None => normal,
            };
            single(res)
        }
        "FADD" | "FSUB" | "FMUL" | "FDIV" | "FMA" | "FNEG" | "FABS" | "FMIN" | "FMAX"
        | "FCLAMP" | "FCOPYSIGN" | "FSQRT" => {
            let ty = &op.results[0];
            let (eb, sb) = fp_format(ty).expect("float op type");
            let f = |i: usize| match &arg_syms[i] {
                SymVal::Float { expr, .. } => expr.clone(),
                _ => unreachable!(),
            };
            match info.name {
                "FADD" => single(SymVal::Float {
                    expr: format!("(fp.add RNE {} {})", f(0), f(1)),
                    ty: ty.clone(),
                }),
                "FSUB" => single(SymVal::Float {
                    expr: format!("(fp.sub RNE {} {})", f(0), f(1)),
                    ty: ty.clone(),
                }),
                "FMUL" => single(SymVal::Float {
                    expr: format!("(fp.mul RNE {} {})", f(0), f(1)),
                    ty: ty.clone(),
                }),
                "FDIV" => single(SymVal::Float {
                    expr: format!("(fp.div RNE {} {})", f(0), f(1)),
                    ty: ty.clone(),
                }),
                "FMA" => single(SymVal::Float {
                    expr: format!("(fp.fma RNE {} {} {})", f(0), f(1), f(2)),
                    ty: ty.clone(),
                }),
                "FNEG" => single(SymVal::Float {
                    expr: format!("(fp.neg {})", f(0)),
                    ty: ty.clone(),
                }),
                "FABS" => single(SymVal::Float {
                    expr: format!("(fp.abs {})", f(0)),
                    ty: ty.clone(),
                }),
                "FMIN" => single(SymVal::Float {
                    expr: smt_fmin(&f(0), &f(1), eb, sb),
                    ty: ty.clone(),
                }),
                "FMAX" => single(SymVal::Float {
                    expr: smt_fmax(&f(0), &f(1), eb, sb),
                    ty: ty.clone(),
                }),
                "FCLAMP" => {
                    let x = f(0);
                    let lo = f(1);
                    let hi = f(2);
                    // Bind intermediate FMAX(x, lo) to avoid 25x exponential duplication.
                    let tmp = qb.fresh_sym(
                        &format!("{prefix}_fclamp"),
                        &format!("(_ FloatingPoint {eb} {sb})"),
                    );
                    let fmax_expr = smt_fmax(&x, &lo, eb, sb);
                    let mut syms = BTreeSet::new();
                    syms.insert(tmp.clone());
                    qb.add_pure_def(syms, format!("(= {tmp} {fmax_expr})"));
                    let clamped = SymVal::Float {
                        expr: smt_fmin(&tmp, &hi, eb, sb),
                        ty: ty.clone(),
                    };
                    let dom = format!(
                        "(and (not (fp.isNaN {lo})) (not (fp.isNaN {hi})) (fp.leq {lo} {hi}))"
                    );
                    single(guard_partial(qb, &dom, clamped, ty, arg_syms, "FCLAMP"))
                }
                "FCOPYSIGN" => {
                    let x = f(0);
                    let y = f(1);
                    let cs = SymVal::Float {
                        expr: smt_fcopysign(&x, &y, eb, sb),
                        ty: ty.clone(),
                    };
                    let dom = format!("(not (fp.isNaN {y}))");
                    single(guard_partial(qb, &dom, cs, ty, arg_syms, "FCOPYSIGN"))
                }
                "FSQRT" => single(SymVal::Float {
                    expr: format!("(fp.sqrt RNE {})", f(0)),
                    ty: ty.clone(),
                }),
                _ => unreachable!(),
            }
        }
        "EQ" | "NE" => {
            let ty = node.operand_type(op.operands[0]).expect("eq operand type");
            let eq = match (ty, &arg_syms[0], &arg_syms[1]) {
                (Type::Bool, SymVal::Bool(a), SymVal::Bool(b)) => format!("(= {a} {b})"),
                (
                    Type::UInt(_) | Type::SInt(_),
                    SymVal::Int { expr: a, .. },
                    SymVal::Int { expr: b, .. },
                ) => {
                    format!("(= {a} {b})")
                }
                (
                    Type::Float(_) | Type::BFloat16,
                    SymVal::Float { expr: a, .. },
                    SymVal::Float { expr: b, .. },
                ) => {
                    format!("(fp.eq {a} {b})")
                }
                (Type::Handle(_), SymVal::Handle(a), SymVal::Handle(b)) => format!("(= {a} {b})"),
                _ => {
                    let mut dummy_env = FormulaEnv {
                        node,
                        callee,
                        inputs: Vec::new(),
                        results: Vec::new(),
                        ports: BTreeMap::new(),
                        bound: Vec::new(),
                        resources: None,
                    };
                    encode_val_eq_assume(ty, &arg_syms[0], &arg_syms[1], false, &mut dummy_env, qb)
                }
            };
            let res = if info.name == "NE" {
                format!("(not {eq})")
            } else {
                eq
            };
            single(SymVal::Bool(res))
        }
        "LT" | "LE" | "GT" | "GE" => {
            let op_str = match (info.name, op.mode) {
                ("LT", 1) => "bvslt",
                ("LE", 1) => "bvsle",
                ("GT", 1) => "bvsgt",
                ("GE", 1) => "bvsge",
                ("LT", 2) => "bvult",
                ("LE", 2) => "bvule",
                ("GT", 2) => "bvugt",
                ("GE", 2) => "bvuge",
                ("LT", 3) => "fp.lt",
                ("LE", 3) => "fp.leq",
                ("GT", 3) => "fp.gt",
                ("GE", 3) => "fp.geq",
                _ => unreachable!("invalid compare mode"),
            };
            let a = &symval_atoms(&arg_syms[0])[0];
            let b = &symval_atoms(&arg_syms[1])[0];
            single(SymVal::Bool(format!("({op_str} {a} {b})")))
        }
        "BAND" => {
            let (SymVal::Bool(a), SymVal::Bool(b)) = (&arg_syms[0], &arg_syms[1]) else {
                unreachable!()
            };
            single(SymVal::Bool(format!("(and {a} {b})")))
        }
        "BOR" => {
            let (SymVal::Bool(a), SymVal::Bool(b)) = (&arg_syms[0], &arg_syms[1]) else {
                unreachable!()
            };
            single(SymVal::Bool(format!("(or {a} {b})")))
        }
        "BNOT" => {
            let SymVal::Bool(a) = &arg_syms[0] else {
                unreachable!()
            };
            single(SymVal::Bool(format!("(not {a})")))
        }
        "INDEX" => {
            let SymVal::Int { expr: idx, .. } = &arg_syms[1] else {
                unreachable!()
            };
            qb.record_index(&arg_syms[0], vec![idx.clone()], idx.clone());
            let sel = symval_select(&arg_syms[0], idx, &mut qb.next_coll_id);
            qb.register_coll_id(&sel);
            let len64 = coll_len_bv64(&arg_syms[0]);
            let dom = format!("(bvult {idx} {len64})");
            let res = guard_partial(qb, &dom, sel, &op.results[0], arg_syms, "INDEX");
            single(res)
        }
        "UPDATE" => {
            let SymVal::Int { expr: idx, .. } = &arg_syms[1] else {
                unreachable!()
            };
            qb.record_index(&arg_syms[0], vec![idx.clone()], idx.clone());
            let upd = symval_store(&arg_syms[0], idx, &arg_syms[2], &mut qb.next_coll_id);
            qb.register_coll_id(&upd);
            if let (Some(dst), Some(src)) = (upd.coll_id(), arg_syms[0].coll_id()) {
                qb.coll_links.push(CollLink::Identity { dst, src });
            }
            let len64 = coll_len_bv64(&arg_syms[0]);
            let dom = format!("(bvult {idx} {len64})");
            let res = guard_partial(qb, &dom, upd, &op.results[0], arg_syms, "UPDATE");
            single(res)
        }
        "LEN" => single(SymVal::Int {
            expr: coll_len_bv64(&arg_syms[0]),
            ty: Type::UInt(64),
        }),
        "SLICE" => {
            let SymVal::Int { expr: lo, .. } = &arg_syms[1] else {
                unreachable!()
            };
            let SymVal::Int { expr: hi, .. } = &arg_syms[2] else {
                unreachable!()
            };
            let src_len = coll_len_bv64(&arg_syms[0]);
            let slice_len = format!("(bvsub {hi} {lo})");
            let Type::Slice(elem_ty) = &op.results[0] else {
                unreachable!()
            };
            let src_arrays = arg_syms[0].arrays().expect("slice src arrays").to_vec();
            let normal = if lo == "#x0000000000000000" || lo == "(_ bv0 64)" {
                let cid = qb.intern_coll(Some(slice_len.clone()), &src_arrays);
                if let Some(src_id) = arg_syms[0].coll_id()
                    && cid != src_id
                {
                    qb.coll_links.push(CollLink::Identity {
                        dst: cid,
                        src: src_id,
                    });
                }
                SymVal::Slice {
                    coll_id: cid,
                    len: slice_len,
                    elem_ty: (**elem_ty).clone(),
                    arrays: src_arrays,
                }
            } else {
                let res_sv = qb.fresh_symval(&format!("{prefix}_slice"), &op.results[0]);
                let SymVal::Slice {
                    coll_id: dst_coll,
                    arrays: dst_arrays,
                    ..
                } = &res_sv
                else {
                    unreachable!()
                };
                if let Some(src_id) = arg_syms[0].coll_id() {
                    qb.coll_links.push(CollLink::Offset {
                        dst: *dst_coll,
                        src: src_id,
                        offset: lo.clone(),
                    });
                }
                qb.pending_schemas.push(ElemSchema::Slice {
                    dst_coll: *dst_coll,
                    dst_arrays: dst_arrays.clone(),
                    src_arrays,
                    slice_len: slice_len.clone(),
                    lo: lo.clone(),
                });
                SymVal::Slice {
                    coll_id: *dst_coll,
                    len: slice_len,
                    elem_ty: (**elem_ty).clone(),
                    arrays: dst_arrays.clone(),
                }
            };
            let dom = format!("(and (bvule {lo} {hi}) (bvule {hi} {src_len}))");
            let res = guard_partial(qb, &dom, normal, &op.results[0], arg_syms, "SLICE");
            single(res)
        }
        "CONCAT" => {
            let la = coll_len_bv64(&arg_syms[0]);
            let lb = coll_len_bv64(&arg_syms[1]);
            let res_sv = qb.fresh_symval(&format!("{prefix}_concat"), &op.results[0]);
            let dst_coll = res_sv.coll_id().expect("concat coll id");
            let dst_arrays = res_sv.arrays().expect("concat arrays").to_vec();
            let a_arrays = arg_syms[0].arrays().expect("a arrays").to_vec();
            let b_arrays = arg_syms[1].arrays().expect("b arrays").to_vec();
            if let (Some(a_id), Some(b_id)) = (arg_syms[0].coll_id(), arg_syms[1].coll_id()) {
                qb.coll_links.push(CollLink::Concat {
                    dst: dst_coll,
                    a: a_id,
                    b: b_id,
                    len_a: la.clone(),
                });
            }
            qb.pending_schemas.push(ElemSchema::Concat {
                dst_coll,
                dst_arrays: dst_arrays.clone(),
                a_arrays,
                b_arrays,
                len_a: la.clone(),
                len_b: lb.clone(),
            });
            let normal = match &op.results[0] {
                Type::Array(elem_ty, n) => SymVal::Array {
                    coll_id: dst_coll,
                    len: u64::from(*n),
                    elem_ty: (**elem_ty).clone(),
                    arrays: dst_arrays,
                },
                Type::Slice(elem_ty) => SymVal::Slice {
                    coll_id: dst_coll,
                    len: format!("(bvadd {la} {lb})"),
                    elem_ty: (**elem_ty).clone(),
                    arrays: dst_arrays,
                },
                _ => unreachable!(),
            };
            if matches!(op.results[0], Type::Slice(_)) {
                let dom = format!(
                    "(= ((_ extract 64 64) (bvadd {} {})) #b0)",
                    zext(&la, 64, 65),
                    zext(&lb, 64, 65)
                );
                single(guard_partial(
                    qb,
                    &dom,
                    normal,
                    &op.results[0],
                    arg_syms,
                    "CONCAT",
                ))
            } else {
                single(normal)
            }
        }
        "SIMD_MAP" => {
            let Operand::Callee(cix) = op.operands[0] else {
                unreachable!()
            };
            let kernel_hash = node.calls[cix as usize];
            let Some(kernel) = callee(&kernel_hash) else {
                qb.unresolved_error.get_or_insert_with(|| {
                    format!(
                        "missing kernel {} in SIMD_MAP",
                        vg_core::ir::hex(&kernel_hash)
                    )
                });
                return vec![qb.fresh_symval(&format!("{prefix}_map_miss"), &op.results[0])];
            };
            let src_val = arg_syms[1].clone();
            let src_ty = node
                .operand_type(op.operands[1])
                .expect("simd src ty")
                .clone();
            let broadcasts: Vec<(Type, SymVal)> = op.operands[2..]
                .iter()
                .zip(&arg_syms[2..])
                .map(|(o, sv)| (node.operand_type(*o).expect("bcast ty").clone(), sv.clone()))
                .collect();
            let out_base = qb.fresh_symval(&format!("{prefix}_map"), &op.results[0]);
            let dst_arrays = out_base.arrays().expect("map arrays").to_vec();
            let res_sv = match &op.results[0] {
                Type::Slice(u) => {
                    let len = coll_len_bv64(&src_val);
                    let cid = qb.intern_coll(Some(len.clone()), &dst_arrays);
                    SymVal::Slice {
                        coll_id: cid,
                        len,
                        elem_ty: (**u).clone(),
                        arrays: dst_arrays.clone(),
                    }
                }
                _ => out_base,
            };
            let dst_coll = res_sv.coll_id().expect("map dst coll");
            if let Some(src_coll) = src_val.coll_id() {
                qb.coll_links.push(CollLink::Identity {
                    dst: dst_coll,
                    src: src_coll,
                });
            }
            qb.pending_schemas.push(ElemSchema::SimdMap {
                dst_coll,
                dst_ty: op.results[0].clone(),
                dst_arrays,
                src_val,
                src_ty,
                broadcasts,
                kernel,
                kernel_hash,
            });
            single(res_sv)
        }
        "GENERATE" => {
            let Operand::Callee(cix) = op.operands[0] else {
                unreachable!()
            };
            let kernel_hash = node.calls[cix as usize];
            let Some(kernel) = callee(&kernel_hash) else {
                qb.unresolved_error.get_or_insert_with(|| {
                    format!(
                        "missing kernel {} in GENERATE",
                        vg_core::ir::hex(&kernel_hash)
                    )
                });
                return vec![qb.fresh_symval(&format!("{prefix}_gen_miss"), &op.results[0])];
            };
            let SymVal::Int {
                expr: count_bv64, ..
            } = &arg_syms[1]
            else {
                unreachable!()
            };
            let broadcasts: Vec<(Type, SymVal)> = op.operands[2..]
                .iter()
                .zip(&arg_syms[2..])
                .map(|(o, sv)| (node.operand_type(*o).expect("bcast ty").clone(), sv.clone()))
                .collect();
            let out_base = qb.fresh_symval(&format!("{prefix}_gen"), &op.results[0]);
            let dst_arrays = out_base.arrays().expect("gen arrays").to_vec();
            let res_sv = match &op.results[0] {
                Type::Slice(u) => {
                    let cid = qb.intern_coll(Some(count_bv64.clone()), &dst_arrays);
                    SymVal::Slice {
                        coll_id: cid,
                        len: count_bv64.clone(),
                        elem_ty: (**u).clone(),
                        arrays: dst_arrays.clone(),
                    }
                }
                _ => out_base,
            };
            let dst_coll = res_sv.coll_id().expect("gen dst coll");
            qb.pending_schemas.push(ElemSchema::Generate {
                dst_coll,
                dst_ty: op.results[0].clone(),
                dst_arrays,
                count_bv64: count_bv64.clone(),
                broadcasts,
                kernel,
                kernel_hash,
            });
            single(res_sv)
        }
        "FOLD_BOUNDED" => {
            // Only reached inside `inline_pure_node` (ADR 0008: FOLD_BOUNDED in SPEC body becomes `fold:`).
            let Operand::Callee(cix) = op.operands[0] else {
                unreachable!()
            };
            let k = op.operands.len();
            let kernel_hash = node.calls[cix as usize];
            let Some(kernel) = callee(&kernel_hash) else {
                qb.unresolved_error.get_or_insert_with(|| {
                    format!(
                        "missing kernel {} in FOLD_BOUNDED",
                        vg_core::ir::hex(&kernel_hash)
                    )
                });
                return vec![qb.fresh_symval(&format!("{prefix}_fold_miss"), &op.results[0])];
            };
            let xs_sv = &arg_syms[1];
            let xs_len = coll_len_bv64(xs_sv);
            let k65 = zext(&xs_len, 64, 65);
            let init_sv = &arg_syms[k - 2];
            let b_vals: Vec<(Type, SymVal)> = op.operands[2..k - 2]
                .iter()
                .zip(&arg_syms[2..k - 2])
                .map(|(o, sv)| {
                    (
                        node.operand_type(*o).expect("fold bcast ty").clone(),
                        sv.clone(),
                    )
                })
                .collect();
            let fold_res = apply_fold_values(
                &kernel,
                kernel_hash,
                init_sv,
                xs_sv,
                &k65,
                &b_vals,
                node,
                callee,
                qb,
                unfold_depth,
            );
            single(fold_res)
        }
        "CALL_HASH" => {
            let Operand::Callee(cix) = op.operands[0] else {
                unreachable!()
            };
            let cal_hash = node.calls[cix as usize];
            let Some(cal_node) = callee(&cal_hash) else {
                qb.unresolved_error.get_or_insert_with(|| {
                    format!(
                        "missing callee {} in CALL_HASH",
                        vg_core::ir::hex(&cal_hash)
                    )
                });
                return op
                    .results
                    .iter()
                    .enumerate()
                    .map(|(s, t)| qb.fresh_symval(&format!("{prefix}_call_{s}"), t))
                    .collect();
            };
            let res_syms: Vec<SymVal> = op
                .results
                .iter()
                .enumerate()
                .map(|(s, t)| qb.fresh_symval(&format!("{prefix}_call_{s}"), t))
                .collect();
            let call_args: Vec<(Type, SymVal)> = cal_node
                .params
                .iter()
                .cloned()
                .zip(arg_syms[1..].iter().cloned())
                .collect();
            let call_res: Vec<(Type, SymVal)> = cal_node
                .results
                .iter()
                .cloned()
                .zip(res_syms.iter().cloned())
                .collect();
            let mut c_env = FormulaEnv {
                node: &cal_node,
                callee,
                inputs: call_args,
                results: call_res,
                ports: BTreeMap::new(),
                bound: Vec::new(),
                resources: None,
            };
            let pre_ok = assume_conjuncts(&cal_node.contract.pre, &mut c_env, qb);
            let post_ok = assume_conjuncts(&cal_node.contract.post, &mut c_env, qb);
            if !cal_node.contract.post.is_empty() {
                if pre_ok == "true" {
                    qb.assertions.push(post_ok);
                } else {
                    qb.assertions.push(format!("(=> {pre_ok} {post_ok})"));
                }
            }
            res_syms
        }
        "ASSERT_GUARD" => {
            let SymVal::Bool(cond) = &arg_syms[0] else {
                unreachable!()
            };
            let Operand::Callee(t_ix) = op.operands[1] else {
                unreachable!()
            };
            let Operand::Callee(e_ix) = *op.operands.last().expect("else branch") else {
                unreachable!()
            };
            let th = node.calls[t_ix as usize];
            let eh = node.calls[e_ix as usize];
            let (Some(tn), Some(en)) = (callee(&th), callee(&eh)) else {
                qb.unresolved_error
                    .get_or_insert_with(|| "missing branch callee in ASSERT_GUARD".to_string());
                return op
                    .results
                    .iter()
                    .enumerate()
                    .map(|(s, t)| qb.fresh_symval(&format!("{prefix}_guard_{s}"), t))
                    .collect();
            };
            let res_syms: Vec<SymVal> = op
                .results
                .iter()
                .enumerate()
                .map(|(s, t)| qb.fresh_symval(&format!("{prefix}_guard_{s}"), t))
                .collect();
            let b_args: Vec<SymVal> = arg_syms[2..arg_syms.len() - 1].to_vec();
            for (branch_cond, bnode) in [(cond.clone(), tn), (format!("(not {cond})"), en)] {
                let inputs: Vec<(Type, SymVal)> = bnode
                    .params
                    .iter()
                    .cloned()
                    .zip(b_args.iter().cloned())
                    .collect();
                let results: Vec<(Type, SymVal)> = bnode
                    .results
                    .iter()
                    .cloned()
                    .zip(res_syms.iter().cloned())
                    .collect();
                let mut b_env = FormulaEnv {
                    node: &bnode,
                    callee,
                    inputs,
                    results,
                    ports: BTreeMap::new(),
                    bound: Vec::new(),
                    resources: None,
                };
                let pre_ok = assume_conjuncts(&bnode.contract.pre, &mut b_env, qb);
                let post_ok = assume_conjuncts(&bnode.contract.post, &mut b_env, qb);
                if !bnode.contract.post.is_empty() {
                    qb.assertions
                        .push(format!("(=> (and {branch_cond} {pre_ok}) {post_ok})"));
                }
            }
            res_syms
        }
        other => {
            qb.unresolved_error
                .get_or_insert_with(|| format!("unsupported op `{other}` in pure inline"));
            op.results
                .iter()
                .enumerate()
                .map(|(s, t)| qb.fresh_symval(&format!("{prefix}_unsup_{s}"), t))
                .collect()
        }
    }
}

pub fn assume_conjuncts(terms: &[Term], env: &mut FormulaEnv<'_>, qb: &mut QueryBuilder) -> String {
    if terms.is_empty() {
        return "true".to_string();
    }
    let parts: Vec<String> = terms.iter().map(|t| encode_assume(t, env, qb)).collect();
    if parts.len() == 1 {
        parts.into_iter().next().expect("1")
    } else {
        format!("(and {})", parts.join(" "))
    }
}

pub fn coll_len_bv64(sv: &SymVal) -> String {
    match sv {
        SymVal::Array { len, .. } => bv_lit_u128(u128::from(*len), 64),
        SymVal::Tensor { dims, .. } => {
            let tot: u64 = dims
                .iter()
                .fold(1u64, |acc, &d| acc.saturating_mul(u64::from(d)));
            bv_lit_u128(u128::from(tot), 64)
        }
        SymVal::Slice { len, .. } => len.clone(),
        _ => panic!("coll_len_bv64 on non-collection {sv:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_fold_values(
    kernel: &Node,
    kernel_hash: Hash,
    init_sv: &SymVal,
    xs_sv: &SymVal,
    k65: &str,
    b_vals: &[(Type, SymVal)],
    env_node: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> SymVal {
    let acc_ty = kernel.params[0].clone();
    let elem_ty = kernel.params[1].clone();
    let xs_len_bv64 = coll_len_bv64(xs_sv);
    let xs_slice_ty = Type::Slice(Box::new(elem_ty.clone()));
    let xs_as_slice = SymVal::Slice {
        coll_id: xs_sv.coll_id().unwrap_or(0),
        len: xs_len_bv64.clone(),
        elem_ty: elem_ty.clone(),
        arrays: xs_sv.arrays().expect("xs arrays").to_vec(),
    };
    let mut flat_sorts = Vec::new();
    let mut flat_args = Vec::new();
    for (_, s) in flatten_type(&acc_ty) {
        flat_sorts.push(s.to_string());
    }
    flat_args.extend(symval_atoms(init_sv));
    for (_, s) in flatten_type(&xs_slice_ty) {
        flat_sorts.push(s.to_string());
    }
    flat_args.extend(symval_atoms(&xs_as_slice));
    flat_sorts.push("(_ BitVec 65)".to_string());
    flat_args.push(k65.to_string());
    for (bt, bv) in b_vals {
        for (_, s) in flatten_type(bt) {
            flat_sorts.push(s.to_string());
        }
        flat_args.extend(symval_atoms(bv));
    }
    let hex12 = &vg_core::ir::hex(&kernel_hash)[..12];
    let out_sorts = flatten_type(&acc_ty);
    let mut atoms = Vec::with_capacity(out_sorts.len());
    for (c, (_, s)) in out_sorts.into_iter().enumerate() {
        let uf = format!("fold_{hex12}_{c}");
        qb.declare_fun_once(&uf, flat_sorts.clone(), s.to_string());
        atoms.push(format!("({uf} {})", flat_args.join(" ")));
    }
    let res_sym = symval_from_atoms(&acc_ty, &atoms, &mut qb.next_coll_id);
    qb.register_coll_id(&res_sym);

    let key = format!("{hex12}:{}", flat_args.join(","));
    if qb.fold_apps.insert(key) {
        let zero65 = bv_lit_i128(0, 65);
        let one65 = bv_lit_i128(1, 65);
        if let Some(eq_base) =
            try_scalar_or_val_same(&acc_ty, &res_sym, init_sv, env_node, callee, qb)
        {
            qb.assertions
                .push(format!("(=> (= {k65} {zero65}) {eq_base})"));
        }
        let k_minus_1 = format!("(bvsub {k65} {one65})");
        let mut prev_args = Vec::new();
        prev_args.extend(symval_atoms(init_sv));
        prev_args.extend(symval_atoms(&xs_as_slice));
        prev_args.push(k_minus_1.clone());
        for (_, bv) in b_vals {
            prev_args.extend(symval_atoms(bv));
        }
        let prev_atoms: Vec<String> = (0..flatten_type(&acc_ty).len())
            .map(|c| format!("(fold_{hex12}_{c} {})", prev_args.join(" ")))
            .collect();
        let prev_acc = symval_from_atoms(&acc_ty, &prev_atoms, &mut qb.next_coll_id);
        let idx_bv64 = extract_lo(&k_minus_1, 65, 64);
        let elem_sym = symval_select(xs_sv, &idx_bv64, &mut qb.next_coll_id);
        qb.register_coll_id(&elem_sym);
        if let Some(cid) = xs_sv.coll_id() {
            qb.index_pool.entry(cid).or_default().insert(IndexAccess {
                dims: vec![idx_bv64.clone()],
                flat: idx_bv64,
            });
        }
        let mut step_args = Vec::with_capacity(2 + b_vals.len());
        step_args.push((acc_ty.clone(), prev_acc));
        step_args.push((elem_ty, elem_sym));
        step_args.extend(b_vals.iter().cloned());
        if let Some(step_res) = inline_pure_node(
            kernel,
            kernel_hash,
            &step_args,
            &format!("fld_{hex12}"),
            callee,
            qb,
            unfold_depth + 1,
        ) && let Some(step_out) = step_res.first()
            && let Some(eq_step) =
                try_scalar_or_val_same(&acc_ty, &res_sym, step_out, kernel, callee, qb)
        {
            let len65 = zext(&xs_len_bv64, 64, 65);
            qb.assertions.push(format!(
                "(=> (and (bvsgt {k65} {zero65}) (bvsle {k65} {len65})) {eq_step})"
            ));
        }
    }
    res_sym
}

/// Inlines a pure `Node` (kernel or `SPEC` body) in `DomainMode::PartialGuarded`.
pub fn inline_pure_node(
    target: &Node,
    _hash: Hash,
    args: &[(Type, SymVal)],
    prefix_hint: &str,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    qb: &mut QueryBuilder,
    unfold_depth: u32,
) -> Option<Vec<SymVal>> {
    if !target.effects.is_pure() || !target.learnable.is_empty() {
        return None;
    }
    let tag_id = qb.next_fresh;
    qb.next_fresh += 1;
    let base_prefix = format!("{prefix_hint}_{tag_id}");

    let mut const_vals = Vec::with_capacity(target.consts.len());
    for (ci, c) in target.consts.iter().enumerate() {
        if c.ty == Type::Int {
            // Int constants only appear in CONTRACT, not ops.
            const_vals.push(SymVal::Eff);
        } else {
            let sv = const_to_symval(
                c,
                &format!("{base_prefix}_c{ci}"),
                &mut qb.decls,
                &mut qb.assertions,
                &mut qb.next_coll_id,
            );
            qb.register_coll_id(&sv);
            const_vals.push(sv);
        }
    }

    let mut op_results: Vec<Vec<SymVal>> = Vec::with_capacity(target.ops.len());
    let resolve = |o: Operand, op_results: &[Vec<SymVal>]| -> SymVal {
        match o {
            Operand::Input(i) => args[i as usize].1.clone(),
            Operand::Op(i, s) => op_results[i as usize][s as usize].clone(),
            Operand::Const(c) => const_vals[c as usize].clone(),
            Operand::Callee(_) | Operand::Invariant(_) => SymVal::Eff,
        }
    };

    for (oi, op) in target.ops.iter().enumerate() {
        let arg_syms: Vec<SymVal> = op
            .operands
            .iter()
            .map(|o| resolve(*o, &op_results))
            .collect();
        let op_prefix = format!("{base_prefix}_o{oi}");
        let rhs_syms = eval_op_core(
            target,
            op,
            &arg_syms,
            &op_prefix,
            DomainMode::PartialGuarded,
            callee,
            qb,
            unfold_depth,
        );
        // Bind each op result to fresh named SMT constants for DAG sharing.
        let mut bound_syms = Vec::with_capacity(op.results.len());
        for (slot, (res_ty, rhs)) in op.results.iter().zip(rhs_syms).enumerate() {
            let sym_name = format!("{op_prefix}_{slot}");
            let declared = declare_symval(&sym_name, res_ty, &mut qb.decls, &mut qb.next_coll_id);
            let lhs_atoms = symval_atoms(&declared);
            qb.bind_pure_symval(&lhs_atoms, &rhs);
            let sv = match (res_ty, &rhs) {
                (Type::Slice(elem), SymVal::Slice { len, .. }) => {
                    let arrays = declared.arrays().expect("slice arrays").to_vec();
                    let cid = qb.intern_coll(Some(len.clone()), &arrays);
                    if let Some(rcid) = rhs.coll_id()
                        && rcid != cid
                    {
                        qb.coll_links.push(CollLink::Identity {
                            dst: cid,
                            src: rcid,
                        });
                    }
                    // Bind the declared arrays directly without changing `len` formula.
                    let _ = len;
                    let _ = elem;
                    declared
                }
                _ => {
                    if let (Some(dst_c), Some(src_c)) = (declared.coll_id(), rhs.coll_id())
                        && dst_c != src_c
                    {
                        qb.coll_links.push(CollLink::Identity {
                            dst: dst_c,
                            src: src_c,
                        });
                    }
                    declared
                }
            };
            qb.register_coll_id(&sv);
            bound_syms.push(sv);
        }
        op_results.push(bound_syms);
    }

    let outs = target
        .outputs
        .iter()
        .map(|o| resolve(*o, &op_results))
        .collect();
    Some(outs)
}

/// Encodes `CAST.<S2>(v)` returning `(normal_symval, optional_domain_obligation_condition)`.
pub fn encode_cast(src_ty: &Type, dst_ty: &Type, src: &SymVal) -> (SymVal, Option<String>) {
    match (src_ty, dst_ty) {
        (Type::Bool, Type::UInt(w) | Type::SInt(w)) => {
            let SymVal::Bool(b) = src else { unreachable!() };
            let w32 = u32::from(*w);
            (
                SymVal::Int {
                    expr: format!("(ite {b} {} {})", bv_lit_u128(1, w32), bv_lit_u128(0, w32)),
                    ty: dst_ty.clone(),
                },
                None,
            )
        }
        (Type::UInt(m), Type::UInt(n)) => {
            let SymVal::Int { expr: x, .. } = src else {
                unreachable!()
            };
            let (m32, n32) = (u32::from(*m), u32::from(*n));
            if n32 >= m32 {
                (
                    SymVal::Int {
                        expr: zext(x, m32, n32),
                        ty: dst_ty.clone(),
                    },
                    None,
                )
            } else {
                let max_n = (1u128 << n32) - 1;
                (
                    SymVal::Int {
                        expr: extract_lo(x, m32, n32),
                        ty: dst_ty.clone(),
                    },
                    Some(format!("(bvule {x} {})", bv_lit_u128(max_n, m32))),
                )
            }
        }
        (Type::UInt(m), Type::SInt(n)) => {
            let SymVal::Int { expr: x, .. } = src else {
                unreachable!()
            };
            let (m32, n32) = (u32::from(*m), u32::from(*n));
            if n32 > m32 {
                (
                    SymVal::Int {
                        expr: zext(x, m32, n32),
                        ty: dst_ty.clone(),
                    },
                    None,
                )
            } else {
                let max_n = (1u128 << (n32 - 1)) - 1;
                (
                    SymVal::Int {
                        expr: extract_lo(x, m32, n32),
                        ty: dst_ty.clone(),
                    },
                    Some(format!("(bvule {x} {})", bv_lit_u128(max_n, m32))),
                )
            }
        }
        (Type::SInt(m), Type::SInt(n)) => {
            let SymVal::Int { expr: x, .. } = src else {
                unreachable!()
            };
            let (m32, n32) = (u32::from(*m), u32::from(*n));
            if n32 >= m32 {
                (
                    SymVal::Int {
                        expr: sext(x, m32, n32),
                        ty: dst_ty.clone(),
                    },
                    None,
                )
            } else {
                let min_n = -(1i128 << (n32 - 1));
                let max_n = (1i128 << (n32 - 1)) - 1;
                (
                    SymVal::Int {
                        expr: extract_lo(x, m32, n32),
                        ty: dst_ty.clone(),
                    },
                    Some(format!(
                        "(and (bvsge {x} {}) (bvsle {x} {}))",
                        bv_lit_i128(min_n, m32),
                        bv_lit_i128(max_n, m32)
                    )),
                )
            }
        }
        (Type::SInt(m), Type::UInt(n)) => {
            let SymVal::Int { expr: x, .. } = src else {
                unreachable!()
            };
            let (m32, n32) = (u32::from(*m), u32::from(*n));
            if n32 >= m32 {
                (
                    SymVal::Int {
                        expr: zext(x, m32, n32),
                        ty: dst_ty.clone(),
                    },
                    Some(format!("(bvsge {x} {})", bv_lit_i128(0, m32))),
                )
            } else {
                let max_n = (1i128 << n32) - 1;
                (
                    SymVal::Int {
                        expr: extract_lo(x, m32, n32),
                        ty: dst_ty.clone(),
                    },
                    Some(format!(
                        "(and (bvsge {x} {}) (bvsle {x} {}))",
                        bv_lit_i128(0, m32),
                        bv_lit_i128(max_n, m32)
                    )),
                )
            }
        }
        (Type::UInt(_), _) if dst_ty.is_float() => {
            let SymVal::Int { expr: x, .. } = src else {
                unreachable!()
            };
            let (eb, sb) = fp_format(dst_ty).expect("float target");
            (
                SymVal::Float {
                    expr: format!("((_ to_fp_unsigned {eb} {sb}) RNE {x})"),
                    ty: dst_ty.clone(),
                },
                None,
            )
        }
        (Type::SInt(_), _) if dst_ty.is_float() => {
            let SymVal::Int { expr: x, .. } = src else {
                unreachable!()
            };
            let (eb, sb) = fp_format(dst_ty).expect("float target");
            (
                SymVal::Float {
                    expr: format!("((_ to_fp {eb} {sb}) RNE {x})"),
                    ty: dst_ty.clone(),
                },
                None,
            )
        }
        (s, Type::UInt(n)) if s.is_float() => {
            let SymVal::Float { expr: x, .. } = src else {
                unreachable!()
            };
            let dom = smt_float_fits_int(x, s, dst_ty);
            (
                SymVal::Int {
                    expr: format!("((_ fp.to_ubv {n}) RTZ {x})"),
                    ty: dst_ty.clone(),
                },
                Some(dom),
            )
        }
        (s, Type::SInt(n)) if s.is_float() => {
            let SymVal::Float { expr: x, .. } = src else {
                unreachable!()
            };
            let dom = smt_float_fits_int(x, s, dst_ty);
            (
                SymVal::Int {
                    expr: format!("((_ fp.to_sbv {n}) RTZ {x})"),
                    ty: dst_ty.clone(),
                },
                Some(dom),
            )
        }
        (s, d) if s.is_float() && d.is_float() => {
            let SymVal::Float { expr: x, .. } = src else {
                unreachable!()
            };
            let (eb, sb) = fp_format(d).expect("float target");
            (
                SymVal::Float {
                    expr: format!("((_ to_fp {eb} {sb}) RNE {x})"),
                    ty: dst_ty.clone(),
                },
                None,
            )
        }
        _ => panic!("unsupported CAST from {src_ty} to {dst_ty}"),
    }
}

pub fn encode_add_sub_mul(
    name: &str,
    mode: u8,
    ty: &Type,
    a: &str,
    b: &str,
) -> (SymVal, Option<String>) {
    let (w, signed) = match ty {
        Type::UInt(w) => (u32::from(*w), false),
        Type::SInt(w) => (u32::from(*w), true),
        _ => unreachable!("int arith type"),
    };
    let bvop = match name {
        "ADD" => "bvadd",
        "SUB" => "bvsub",
        "MUL" => "bvmul",
        _ => unreachable!(),
    };
    let wrap_expr = format!("({bvop} {a} {b})");
    if mode == 1 {
        // wrap
        return (
            SymVal::Int {
                expr: wrap_expr,
                ty: ty.clone(),
            },
            None,
        );
    }
    let ext_w = if name == "MUL" { w * 2 } else { w + 1 };
    let ext = |e: &str| {
        if signed {
            sext(e, w, ext_w)
        } else {
            zext(e, w, ext_w)
        }
    };
    let exact = format!("({bvop} {} {})", ext(a), ext(b));
    let (in_range, sat_expr) = if signed {
        let min_v = -(1i128 << (w - 1));
        let max_v = (1i128 << (w - 1)) - 1;
        let min_ext = bv_lit_i128(min_v, ext_w);
        let max_ext = bv_lit_i128(max_v, ext_w);
        let min_w = bv_lit_i128(min_v, w);
        let max_w = bv_lit_i128(max_v, w);
        let ok = format!("(and (bvsge {exact} {min_ext}) (bvsle {exact} {max_ext}))");
        let trunc = extract_lo(&exact, ext_w, w);
        let sat = format!(
            "(ite (bvslt {exact} {min_ext}) {min_w} (ite (bvsgt {exact} {max_ext}) {max_w} {trunc}))"
        );
        (ok, sat)
    } else {
        let max_w = bv_lit_u128(
            if w == 64 {
                u64::MAX as u128
            } else {
                (1u128 << w) - 1
            },
            w,
        );
        let zero_w = bv_lit_u128(0, w);
        let ok = if name == "MUL" {
            format!("(= ((_ extract {} {w}) {exact}) {zero_w})", ext_w - 1)
        } else {
            format!("(= ((_ extract {w} {w}) {exact}) #b0)")
        };
        let sat = if name == "SUB" {
            format!("(ite (bvuge {a} {b}) (bvsub {a} {b}) {zero_w})")
        } else {
            let trunc = extract_lo(&exact, ext_w, w);
            format!("(ite {ok} {trunc} {max_w})")
        };
        (ok, sat)
    };
    if mode == 0 {
        // checked
        (
            SymVal::Int {
                expr: wrap_expr,
                ty: ty.clone(),
            },
            Some(in_range),
        )
    } else {
        // sat
        (
            SymVal::Int {
                expr: sat_expr,
                ty: ty.clone(),
            },
            None,
        )
    }
}

pub fn encode_div_mod(name: &str, mode: u8, ty: &Type, a: &str, b: &str) -> (SymVal, String) {
    let (w, signed) = match ty {
        Type::UInt(w) => (u32::from(*w), false),
        Type::SInt(w) => (u32::from(*w), true),
        _ => unreachable!("div/mod int type"),
    };
    let nonzero = format!("(not (= {b} {}))", bv_lit_u128(0, w));
    if !signed {
        let op = if name == "DIV" { "bvudiv" } else { "bvurem" };
        return (
            SymVal::Int {
                expr: format!("({op} {a} {b})"),
                ty: ty.clone(),
            },
            nonzero,
        );
    }
    let min_v = -(1i128 << (w - 1));
    let max_v = (1i128 << (w - 1)) - 1;
    let min_lit = bv_lit_i128(min_v, w);
    let max_lit = bv_lit_i128(max_v, w);
    let neg1_lit = bv_lit_i128(-1, w);
    let is_min_neg1 = format!("(and (= {a} {min_lit}) (= {b} {neg1_lit}))");

    if name == "MOD" {
        return (
            SymVal::Int {
                expr: format!("(ite {is_min_neg1} {} (bvsrem {a} {b}))", bv_lit_u128(0, w)),
                ty: ty.clone(),
            },
            nonzero,
        );
    }
    match mode {
        0 => (
            SymVal::Int {
                expr: format!("(bvsdiv {a} {b})"),
                ty: ty.clone(),
            },
            format!("(and {nonzero} (not {is_min_neg1}))"),
        ),
        1 => (
            SymVal::Int {
                expr: format!("(ite {is_min_neg1} {min_lit} (bvsdiv {a} {b}))"),
                ty: ty.clone(),
            },
            nonzero,
        ),
        _ => (
            SymVal::Int {
                expr: format!("(ite {is_min_neg1} {max_lit} (bvsdiv {a} {b}))"),
                ty: ty.clone(),
            },
            nonzero,
        ),
    }
}

pub fn encode_neg(mode: u8, ty: &Type, a: &str) -> (SymVal, Option<String>) {
    let (w, signed) = match ty {
        Type::UInt(w) => (u32::from(*w), false),
        Type::SInt(w) => (u32::from(*w), true),
        _ => unreachable!("neg int type"),
    };
    if !signed {
        return match mode {
            0 => (
                SymVal::Int {
                    expr: format!("(bvneg {a})"),
                    ty: ty.clone(),
                },
                Some(format!("(= {a} {})", bv_lit_u128(0, w))),
            ),
            1 => (
                SymVal::Int {
                    expr: format!("(bvneg {a})"),
                    ty: ty.clone(),
                },
                None,
            ),
            _ => (
                SymVal::Int {
                    expr: bv_lit_u128(0, w),
                    ty: ty.clone(),
                },
                None,
            ),
        };
    }
    let min_v = -(1i128 << (w - 1));
    let max_v = (1i128 << (w - 1)) - 1;
    let min_lit = bv_lit_i128(min_v, w);
    let max_lit = bv_lit_i128(max_v, w);
    match mode {
        0 => (
            SymVal::Int {
                expr: format!("(bvneg {a})"),
                ty: ty.clone(),
            },
            Some(format!("(not (= {a} {min_lit}))")),
        ),
        1 => (
            SymVal::Int {
                expr: format!("(bvneg {a})"),
                ty: ty.clone(),
            },
            None,
        ),
        _ => (
            SymVal::Int {
                expr: format!("(ite (= {a} {min_lit}) {max_lit} (bvneg {a}))"),
                ty: ty.clone(),
            },
            None,
        ),
    }
}

pub fn encode_shift_rotate(
    name: &str,
    mode: u8,
    ty: &Type,
    a: &str,
    s_u32: &str,
) -> (SymVal, Option<String>) {
    let (w, signed) = match ty {
        Type::UInt(w) => (u32::from(*w), false),
        Type::SInt(w) => (u32::from(*w), true),
        _ => unreachable!("shift int type"),
    };
    let resize_u32_to_w = |s32: String| -> String {
        if w < 32 {
            extract_lo(&s32, 32, w)
        } else if w == 32 {
            s32
        } else {
            zext(&s32, 32, w)
        }
    };
    let w_u32 = bv_lit_u128(u128::from(w), 32);
    match name {
        "SHL" | "SHR" => {
            let bvop = if name == "SHL" {
                "bvshl"
            } else if signed {
                "bvashr"
            } else {
                "bvlshr"
            };
            if mode == 0 {
                let s_w = resize_u32_to_w(s_u32.to_string());
                (
                    SymVal::Int {
                        expr: format!("({bvop} {a} {s_w})"),
                        ty: ty.clone(),
                    },
                    Some(format!("(bvult {s_u32} {w_u32})")),
                )
            } else {
                let s_mod = resize_u32_to_w(format!("(bvurem {s_u32} {w_u32})"));
                (
                    SymVal::Int {
                        expr: format!("({bvop} {a} {s_mod})"),
                        ty: ty.clone(),
                    },
                    None,
                )
            }
        }
        "ROTL" | "ROTR" => {
            let r_u32 = format!("(bvurem {s_u32} {w_u32})");
            let inv_r_u32 = format!("(bvsub {w_u32} {r_u32})");
            let r_w = resize_u32_to_w(r_u32);
            let inv_w = resize_u32_to_w(inv_r_u32);
            let expr = if name == "ROTL" {
                format!("(bvor (bvshl {a} {r_w}) (bvlshr {a} {inv_w}))")
            } else {
                format!("(bvor (bvlshr {a} {r_w}) (bvshl {a} {inv_w}))")
            };
            (
                SymVal::Int {
                    expr,
                    ty: ty.clone(),
                },
                None,
            )
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solver::{CheckResult, Solver, Z3Session};

    fn missing_callee(_: &Hash) -> Option<Node> {
        None
    }

    #[test]
    fn literal_widths_follow_docs_05() {
        assert_eq!(int_literal_width(0), 2);
        assert_eq!(int_literal_width(1), 2);
        assert_eq!(int_literal_width(-1), 2);
        assert_eq!(int_literal_width(127), 8);
        // docs/05: bits(|v|) + 1, so -128 needs 9 bits and i64::MIN needs 65 (wider is sound).
        assert_eq!(int_literal_width(-128), 9);
        assert_eq!(int_literal_width(128), 9);
        assert_eq!(int_literal_width(i64::MIN.into()), 65);
        assert_eq!(int_literal_width(u64::MAX.into()), 65);
    }

    #[test]
    fn formula_div_by_zero_is_unspecified_and_caught_by_wd() {
        let src = r#"
module t;
NODE @f {
  SIGNATURE: (x: i32, y: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {
    POST: result == x / y;
  }
  BODY {
    RETURN(x);
  }
}
"#;
        let asm = vg_core::asm_local(src).unwrap();
        let node = &asm[0].node;
        let wds = collect_wd_obligations(&node.contract.post[0], node, &missing_callee);
        assert_eq!(wds.len(), 1);
        assert!(matches!(wds[0].cond, WdCond::DivisorNonZero(_)));

        // Even with x / y in POST, unguarded y=0 makes WD obligation SAT!
        let mut qb = QueryBuilder::default();
        let x = declare_symval("a0", &Type::SInt(32), &mut qb.decls, &mut qb.next_coll_id);
        let y = declare_symval("a1", &Type::SInt(32), &mut qb.decls, &mut qb.next_coll_id);
        let mut env = FormulaEnv {
            node,
            callee: &missing_callee,
            inputs: vec![(Type::SInt(32), x), (Type::SInt(32), y)],
            results: Vec::new(),
            ports: BTreeMap::new(),
            bound: Vec::new(),
            resources: None,
        };
        let neg = encode_wd_obligation(&wds[0], &mut env, &mut qb);
        qb.assertions.push(neg);
        let q = qb.into_pruned_query();
        let mut sess = Z3Session::new(&crate::find_z3(), 2000);
        assert_eq!(sess.check_query(&q), CheckResult::Sat);

        // Guarded `y != 0 ==> ...` adds path hypothesis y != 0 to WD, making it UNSAT.
        let src_ok = r#"
module t;
NODE @f {
  SIGNATURE: (x: i32, y: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {
    POST: y != 0 ==> result == x / y;
  }
  BODY {
    RETURN(x);
  }
}
"#;
        let asm_ok = vg_core::asm_local(src_ok).unwrap();
        let node_ok = &asm_ok[0].node;
        let wds_ok = collect_wd_obligations(&node_ok.contract.post[0], node_ok, &missing_callee);
        assert_eq!(wds_ok.len(), 1);
        let mut qb2 = QueryBuilder::default();
        let x2 = declare_symval("a0", &Type::SInt(32), &mut qb2.decls, &mut qb2.next_coll_id);
        let y2 = declare_symval("a1", &Type::SInt(32), &mut qb2.decls, &mut qb2.next_coll_id);
        let mut env2 = FormulaEnv {
            node: node_ok,
            callee: &missing_callee,
            inputs: vec![(Type::SInt(32), x2), (Type::SInt(32), y2)],
            results: Vec::new(),
            ports: BTreeMap::new(),
            bound: Vec::new(),
            resources: None,
        };
        let neg2 = encode_wd_obligation(&wds_ok[0], &mut env2, &mut qb2);
        qb2.assertions.push(neg2);
        let q2 = qb2.into_pruned_query();
        assert_eq!(sess.check_query(&q2), CheckResult::Unsat);
    }

    #[test]
    fn spec_pre_guards_post_and_body_soundness_rule_2() {
        let src = r#"
module t;
SPEC @pos_id {
  SIGNATURE: (x: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {
    PRE: x > 0;
    POST: result > 0;
  }
  BODY {
    RETURN(x);
  }
}
NODE @caller {
  SIGNATURE: (x: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {
    POST: spec:pos_id(x) > 0 && x > 0;
  }
  BODY {
    RETURN(x);
  }
}
"#;
        let asm = vg_core::asm_local(src).unwrap();
        let spec_h = asm[0].hashes.node;
        let spec_node = asm[0].node.clone();
        let caller = &asm[1].node;
        let lookup = |h: &Hash| {
            if *h == spec_h {
                Some(spec_node.clone())
            } else {
                None
            }
        };

        let mut qb = QueryBuilder::default();
        let a0 = declare_symval("a0", &Type::SInt(32), &mut qb.decls, &mut qb.next_coll_id);
        let mut env = FormulaEnv {
            node: caller,
            callee: &lookup,
            inputs: vec![(Type::SInt(32), a0.clone())],
            results: vec![(Type::SInt(32), a0)],
            ports: BTreeMap::new(),
            bound: Vec::new(),
            resources: None,
        };
        // Even with spec:pos_id(x) appearing in POST.0, `x > 0` (POST.1) must NOT become provable!
        let _ = encode_neg_goal(&caller.contract.post[0], &mut env, &mut qb);
        let neg1 = encode_neg_goal(&caller.contract.post[1], &mut env, &mut qb);
        qb.assertions.push(neg1);
        let q = qb.into_pruned_query();
        let mut sess = Z3Session::new(&crate::find_z3(), 2000);
        assert_eq!(sess.check_query(&q), CheckResult::Sat);
    }

    #[test]
    fn fold_defining_equations_hold() {
        let src = r#"
module t;
NODE @add_step {
  SIGNATURE: (acc: i32, x: i32) -> (i32);
  EFFECTS: [];
  CONTRACT { }
  BODY {
    %0 = ADD.wrap(acc, x);
    RETURN(%0);
  }
}
NODE @test_fold {
  SIGNATURE: (xs: Array<i32, 4>, init: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {
    PRE: init >= 0 && init <= 100 && xs[0] >= 0 && xs[0] <= 100;
    POST: fold:add_step(init, xs, 0) == init && fold:add_step(init, xs, 1) == init + xs[0];
  }
  BODY {
    RETURN(init);
  }
}
"#;
        let asm = vg_core::asm_local(src).unwrap();
        let step_h = asm[0].hashes.node;
        let step_node = asm[0].node.clone();
        let node = &asm[1].node;
        let lookup = |h: &Hash| {
            if *h == step_h {
                Some(step_node.clone())
            } else {
                None
            }
        };

        let mut qb = QueryBuilder::default();
        let a0 = declare_symval("a0", &node.params[0], &mut qb.decls, &mut qb.next_coll_id);
        let a1 = declare_symval("a1", &node.params[1], &mut qb.decls, &mut qb.next_coll_id);
        qb.register_coll_id(&a0);
        let mut env = FormulaEnv {
            node,
            callee: &lookup,
            inputs: vec![
                (node.params[0].clone(), a0),
                (node.params[1].clone(), a1.clone()),
            ],
            results: vec![(node.results[0].clone(), a1)],
            ports: BTreeMap::new(),
            bound: Vec::new(),
            resources: None,
        };
        for p in &node.contract.pre {
            let s = encode_assume(p, &mut env, &mut qb);
            qb.assertions.push(s);
        }
        let neg0 = encode_neg_goal(&node.contract.post[0], &mut env, &mut qb);
        let neg1 = encode_neg_goal(&node.contract.post[1], &mut env, &mut qb);
        qb.assertions.push(format!("(or {neg0} {neg1})"));
        let q = qb.into_pruned_query();
        let mut sess = Z3Session::new(&crate::find_z3(), 2000);
        assert_eq!(sess.check_query(&q), CheckResult::Unsat);
    }
}
