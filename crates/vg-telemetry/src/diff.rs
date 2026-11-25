//! SMT obligation context, passing-input synthesis, and unsat-core constraint diffs (P6.2).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use vg_core::hash::{Context, derive};
use vg_core::ir::{Hash, Node, Operand, Term};
use vg_core::opcode;
use vg_core::types::Type;
use vg_core::value::Value;
use vg_smt::formula::{
    DomainMode, FVal, FormulaEnv, QueryBuilder, assume_conjuncts, coll_len_bv64,
    collect_wd_obligations, encode_assume, encode_neg_goal, encode_wd_obligation, eval_op_core,
    symval_to_fval, zext,
};
use vg_smt::model::{DecodedModel, decode_with_small_model};
use vg_smt::sexpr::{SExpr, read_sexpr};
use vg_smt::solver::{CheckResult, Solver, Z3Session};
use vg_smt::sort::{
    SymVal, bv_lit_i128, bv_lit_u128, const_to_symval, declare_symval, fp_format, scalar_const_smt,
    symval_atoms, symval_select,
};

use crate::names::Names;
use crate::trace::{ObligationKind, check_violation, parse_obligation_id, trace_eval};

/// Built SMT query state for a specific obligation, supporting:
/// 1. `obligation_hash()` matching `vg-smt`'s canonical query hash;
/// 2. `find_passing_inputs()` for counterfactual delta debugging;
/// 3. Named per-op equations for Z3 unsat-core constraint diffing.
pub struct SmtObligationContext {
    pub inputs: Vec<(Type, SymVal)>,
    pub pre_qb: QueryBuilder,
    pub body_qb: QueryBuilder,
    pub op_syms: Vec<Vec<SymVal>>,
    /// Per-op defining equalities `(defined_syms, eq_expr)` for scalar/bool ops.
    pub op_eqs: BTreeMap<u32, Vec<(BTreeSet<String>, String)>>,
    /// QueryBuilder with `goal_fails` asserted (canonical `vg-smt` style for obligation hash).
    pub fail_qb: Option<QueryBuilder>,
    /// QueryBuilder with `goal_holds` asserted (for passing-input search).
    pub pass_qb: Option<QueryBuilder>,
    /// Raw SMT expressions for `(goal_fails, goal_holds)` on top of `body_qb`.
    pub goal_pair: Option<(String, String)>,
}

fn operand_sym(
    node: &Node,
    o: Operand,
    inputs: &[(Type, SymVal)],
    vals: &[Vec<SymVal>],
    consts: &mut BTreeMap<u32, SymVal>,
    qb: &mut QueryBuilder,
) -> SymVal {
    match o {
        Operand::Input(i) => inputs[i as usize].1.clone(),
        Operand::Op(i, s) => vals[i as usize][s as usize].clone(),
        Operand::Const(c) => consts
            .entry(c)
            .or_insert_with(|| {
                let sv = const_to_symval(
                    &node.consts[c as usize],
                    &format!("c{c}"),
                    &mut qb.decls,
                    &mut qb.assertions,
                    &mut qb.next_coll_id,
                );
                qb.register_coll_id(&sv);
                sv
            })
            .clone(),
        Operand::Callee(_) | Operand::Invariant(_) => SymVal::Bool("true".into()),
    }
}

fn make_env<'a>(
    node: &'a Node,
    callee: &'a dyn Fn(&Hash) -> Option<Node>,
    inputs: Vec<(Type, SymVal)>,
    results: Vec<(Type, SymVal)>,
) -> FormulaEnv<'a> {
    FormulaEnv {
        node,
        callee,
        inputs,
        results,
        ports: BTreeMap::new(),
        bound: Vec::new(),
        resources: None,
    }
}

fn with_defs_of(qb: &QueryBuilder, scratch: &QueryBuilder) -> QueryBuilder {
    let mut q = qb.clone();
    q.decls.extend(scratch.decls.clone());
    q.fun_decls.extend(scratch.fun_decls.clone());
    q.pure_defs = scratch.pure_defs.clone();
    q.next_fresh = scratch.next_fresh;
    q.next_coll_id = scratch.next_coll_id;
    q
}

fn tensor_index(k: &str, dims: &[u32]) -> Vec<String> {
    (0..dims.len())
        .map(|j| {
            let stride: u64 = dims[j + 1..].iter().map(|d| u64::from(*d)).product();
            format!(
                "(bvurem (bvudiv {k} (_ bv{stride} 64)) (_ bv{} 64))",
                dims[j]
            )
        })
        .collect()
}

fn fresh_element(xs: &SymVal, q: &mut QueryBuilder, tag: &str) -> (String, SymVal) {
    let k = q.fresh_sym(tag, "(_ BitVec 64)");
    let len = coll_len_bv64(xs);
    q.assertions.push(format!("(bvult {k} {len})"));
    let dims = match xs {
        SymVal::Tensor { dims, .. } => tensor_index(&k, dims),
        _ => vec![k.clone()],
    };
    q.record_index(xs, dims, k.clone());
    let x = symval_select(xs, &k, &mut q.next_coll_id);
    q.register_coll_id(&x);
    (k, x)
}

fn invariant_smt(
    node: &Node,
    v: u32,
    k: FVal,
    acc: FVal,
    e: &mut FormulaEnv<'_>,
    q: &mut QueryBuilder,
    goal_neg: bool,
) -> String {
    let saved = std::mem::replace(&mut e.bound, vec![k, acc]);
    let terms = &node.contract.invariants[v as usize];
    let s = if goal_neg {
        let negs: Vec<String> = terms.iter().map(|t| encode_neg_goal(t, e, q)).collect();
        match negs.len() {
            0 => "false".into(),
            1 => negs[0].clone(),
            _ => format!("(or {})", negs.join(" ")),
        }
    } else {
        let parts: Vec<String> = terms.iter().map(|t| encode_assume(t, e, q)).collect();
        match parts.len() {
            0 => "true".into(),
            1 => parts[0].clone(),
            _ => format!("(and {})", parts.join(" ")),
        }
    };
    e.bound = saved;
    s
}

fn step_count(expr: String, width: u32) -> FVal {
    FVal::Int {
        expr,
        width,
        orig: None,
    }
}

fn is_scalar_or_bool_ty(ty: &Type) -> bool {
    ty.is_scalar_or_bool() || matches!(ty, Type::Handle(_))
}

impl SmtObligationContext {
    /// Builds the symbolic context for `kind`. When `name_scalar_ops` is true, scalar/bool op
    /// results are bound to explicit `p{i}_{s}` symbols so unsat cores can reference them by name.
    pub fn build(
        node: &Node,
        kind: &ObligationKind,
        callee: &dyn Fn(&Hash) -> Option<Node>,
        name_scalar_ops: bool,
    ) -> Self {
        let mut qb = QueryBuilder::default();
        let inputs: Vec<(Type, SymVal)> = (0..node.input_count() as u32)
            .map(|i| {
                let ty = node.input_type(i).expect("input type").clone();
                let sv = declare_symval(&format!("a{i}"), &ty, &mut qb.decls, &mut qb.next_coll_id);
                qb.register_coll_id(&sv);
                qb.always_keep_decls.extend(symval_atoms(&sv));
                (ty, sv)
            })
            .collect();

        for (t, l) in node.learnable.iter().enumerate() {
            if let SymVal::Float { expr, .. } = &inputs[node.params.len() + t].1 {
                let lo = scalar_const_smt(&l.ty, &node.consts[l.lo as usize].bytes);
                let hi = scalar_const_smt(&l.ty, &node.consts[l.hi as usize].bytes);
                qb.assertions
                    .push(format!("(and (fp.leq {lo} {expr}) (fp.leq {expr} {hi}))"));
            }
        }
        let pre_base = qb.clone();
        {
            let mut e = make_env(node, callee, inputs.clone(), vec![]);
            for t in &node.contract.pre {
                let a = encode_assume(t, &mut e, &mut qb);
                qb.assertions.push(a);
            }
        }
        let pre_qb = qb.clone();

        let mut vals: Vec<Vec<SymVal>> = Vec::new();
        let mut consts = BTreeMap::new();
        let mut ports = BTreeMap::new();
        let mut op_eqs: BTreeMap<u32, Vec<(BTreeSet<String>, String)>> = BTreeMap::new();

        let mut fail_qb: Option<QueryBuilder> = None;
        let mut pass_qb: Option<QueryBuilder> = None;
        let mut goal_pair: Option<(String, String)> = None;

        let target_op = kind.op_index();

        for (i, op) in node.ops.iter().enumerate() {
            let i_u32 = i as u32;
            let info = opcode::by_code(op.opcode).expect("validated opcode");
            let args: Vec<SymVal> = op
                .operands
                .iter()
                .map(|o| operand_sym(node, *o, &inputs, &vals, &mut consts, &mut qb))
                .collect();

            let callee_of = |j: usize| match op.operands.get(j).copied() {
                Some(Operand::Callee(c)) => node.calls.get(c as usize).and_then(callee),
                _ => None,
            };
            let types_of = |target: &Node, xs: &[SymVal]| -> Vec<(Type, SymVal)> {
                target
                    .params
                    .iter()
                    .cloned()
                    .zip(xs.iter().cloned())
                    .collect()
            };

            // Handle obligations attached directly to op `i`.
            if target_op == Some(i_u32) {
                match kind {
                    ObligationKind::OpDomain { kind: dom_kind, .. } => {
                        let mut scratch = qb.clone();
                        scratch.domains.clear();
                        eval_op_core(
                            node,
                            op,
                            &args,
                            &format!("s{i}"),
                            DomainMode::Strict,
                            callee,
                            &mut scratch,
                            0,
                        );
                        let wanted_tag_match = |tag: &str| -> bool {
                            let mapped = match (info.name, tag) {
                                ("ADD" | "SUB" | "MUL" | "NEG", _) => "no_overflow",
                                ("DIV", "no_overflow") => "no_overflow",
                                ("DIV" | "MOD", _) => "nonzero",
                                ("SHL" | "SHR", _) => "amount",
                                ("CAST", _) => "in_range",
                                ("GET_OK", _) => "is_ok",
                                ("GET_ERR", _) => "is_err",
                                ("FCLAMP", _) => "bounds",
                                ("FCOPYSIGN", _) => "sign",
                                ("INDEX" | "UPDATE" | "SLICE", _) => "in_bounds",
                                ("CONCAT", _) => "length",
                                (_, t) => t,
                            };
                            mapped == dom_kind
                        };
                        if let Some((_, dom)) =
                            scratch.domains.iter().find(|(t, _)| wanted_tag_match(t))
                        {
                            let neg = format!("(not {dom})");
                            let mut qf = with_defs_of(&qb, &scratch);
                            qf.assertions.push(neg.clone());
                            let mut qp = with_defs_of(&qb, &scratch);
                            qp.assertions.push(dom.clone());
                            qb = with_defs_of(&qb, &scratch);
                            fail_qb = Some(qf);
                            pass_qb = Some(qp);
                            goal_pair = Some((neg, dom.clone()));
                        }
                    }
                    ObligationKind::CallPre { pre_idx, .. } => {
                        if let Some(f) = callee_of(0)
                            && let Some(t) = f.contract.pre.get(*pre_idx)
                        {
                            let f_args = types_of(&f, &args[1..]);
                            let mut qf = qb.clone();
                            let mut ef = make_env(&f, callee, f_args.clone(), vec![]);
                            let neg = encode_neg_goal(t, &mut ef, &mut qf);
                            qf.assertions.push(neg.clone());

                            let mut qp = qb.clone();
                            let mut ep = make_env(&f, callee, f_args, vec![]);
                            let hold = encode_assume(t, &mut ep, &mut qp);
                            qp.assertions.push(hold.clone());

                            fail_qb = Some(qf);
                            pass_qb = Some(qp);
                            goal_pair = Some((neg, hold));
                        }
                    }
                    ObligationKind::MapPre { pre_idx, .. } => {
                        if let Some(f) = callee_of(0)
                            && let Some(t) = f.contract.pre.get(*pre_idx)
                        {
                            let mut qf = qb.clone();
                            let (_, x) = fresh_element(&args[1], &mut qf, "map_k");
                            let mut a = vec![x];
                            a.extend(args[2..].iter().cloned());
                            let f_args = types_of(&f, &a);
                            let mut ef = make_env(&f, callee, f_args.clone(), vec![]);
                            let neg = encode_neg_goal(t, &mut ef, &mut qf);
                            qf.assertions.push(neg.clone());

                            // For passing inputs, we want ALL elements of xs to satisfy PRE (at least for bounded/sampled or unconstrained).
                            let mut qp = qb.clone();
                            if let SymVal::Array { len, .. } = &args[1] {
                                for idx in 0..(*len).min(16) {
                                    let idx_lit = bv_lit_u128(u128::from(idx), 64);
                                    let el =
                                        symval_select(&args[1], &idx_lit, &mut qp.next_coll_id);
                                    qp.register_coll_id(&el);
                                    let mut ea = vec![el];
                                    ea.extend(args[2..].iter().cloned());
                                    let mut ep = make_env(&f, callee, types_of(&f, &ea), vec![]);
                                    let h = encode_assume(t, &mut ep, &mut qp);
                                    qp.assertions.push(h);
                                }
                            } else {
                                // Slice: require length >= 1 and element 0 satisfies PRE (and elem 1..min(len, 4)).
                                let len_bv = coll_len_bv64(&args[1]);
                                qp.assertions.push(format!("(bvugt {len_bv} (_ bv0 64))"));
                                for idx in 0u128..4 {
                                    let idx_lit = bv_lit_u128(idx, 64);
                                    let el =
                                        symval_select(&args[1], &idx_lit, &mut qp.next_coll_id);
                                    qp.register_coll_id(&el);
                                    let mut ea = vec![el];
                                    ea.extend(args[2..].iter().cloned());
                                    let mut ep = make_env(&f, callee, types_of(&f, &ea), vec![]);
                                    let h = encode_assume(t, &mut ep, &mut qp);
                                    qp.assertions
                                        .push(format!("(=> (bvult {idx_lit} {len_bv}) {h})"));
                                }
                            }

                            fail_qb = Some(qf);
                            pass_qb = Some(qp);
                            goal_pair = Some((neg, "true".into()));
                        }
                    }
                    ObligationKind::GenPre { pre_idx, .. } => {
                        if let Some(f) = callee_of(0)
                            && let Some(t) = f.contract.pre.get(*pre_idx)
                            && let SymVal::Int { expr: n, .. } = &args[1]
                        {
                            let mut qf = qb.clone();
                            let k = qf.fresh_sym("gen_k", "(_ BitVec 64)");
                            qf.assertions.push(format!("(bvult {k} {n})"));
                            let mut a = vec![SymVal::Int {
                                expr: k,
                                ty: Type::UInt(64),
                            }];
                            a.extend(args[2..].iter().cloned());
                            let mut ef = make_env(&f, callee, types_of(&f, &a), vec![]);
                            let neg = encode_neg_goal(t, &mut ef, &mut qf);
                            qf.assertions.push(neg.clone());

                            let mut qp = qb.clone();
                            for idx in 0u128..8 {
                                let idx_lit = bv_lit_u128(idx, 64);
                                let mut a = vec![SymVal::Int {
                                    expr: idx_lit.clone(),
                                    ty: Type::UInt(64),
                                }];
                                a.extend(args[2..].iter().cloned());
                                let mut ep = make_env(&f, callee, types_of(&f, &a), vec![]);
                                let h = encode_assume(t, &mut ep, &mut qp);
                                qp.assertions
                                    .push(format!("(=> (bvult {idx_lit} {n}) {h})"));
                            }

                            fail_qb = Some(qf);
                            pass_qb = Some(qp);
                            goal_pair = Some((neg, "true".into()));
                        }
                    }
                    ObligationKind::GuardThenPre { pre_idx, .. } => {
                        let SymVal::Bool(c) = &args[0] else {
                            unreachable!()
                        };
                        let last = op.operands.len() - 1;
                        if let Some(f) = callee_of(1)
                            && let Some(t) = f.contract.pre.get(*pre_idx)
                        {
                            let b = &args[2..last];
                            let f_args = types_of(&f, b);
                            let mut qf = qb.clone();
                            let mut ef = make_env(&f, callee, f_args.clone(), vec![]);
                            let neg = encode_neg_goal(t, &mut ef, &mut qf);
                            let g_fail = format!("(and {c} {neg})");
                            qf.assertions.push(g_fail.clone());

                            let mut qp = qb.clone();
                            let mut ep = make_env(&f, callee, f_args, vec![]);
                            let h = encode_assume(t, &mut ep, &mut qp);
                            let g_hold = format!("(=> {c} {h})");
                            qp.assertions.push(g_hold.clone());

                            fail_qb = Some(qf);
                            pass_qb = Some(qp);
                            goal_pair = Some((g_fail, g_hold));
                        }
                    }
                    ObligationKind::GuardElsePre { pre_idx, .. } => {
                        let SymVal::Bool(c) = &args[0] else {
                            unreachable!()
                        };
                        let last = op.operands.len() - 1;
                        if let Some(g) = callee_of(last)
                            && let Some(t) = g.contract.pre.get(*pre_idx)
                        {
                            let b = &args[2..last];
                            let g_args = types_of(&g, b);
                            let mut qf = qb.clone();
                            let mut ef = make_env(&g, callee, g_args.clone(), vec![]);
                            let neg = encode_neg_goal(t, &mut ef, &mut qf);
                            let g_fail = format!("(and (not {c}) {neg})");
                            qf.assertions.push(g_fail.clone());

                            let mut qp = qb.clone();
                            let mut ep = make_env(&g, callee, g_args, vec![]);
                            let h = encode_assume(t, &mut ep, &mut qp);
                            let g_hold = format!("(=> (not {c}) {h})");
                            qp.assertions.push(g_hold.clone());

                            fail_qb = Some(qf);
                            pass_qb = Some(qp);
                            goal_pair = Some((g_fail, g_hold));
                        }
                    }
                    ObligationKind::AssertGuard { .. } => {
                        let SymVal::Bool(c) = &args[0] else {
                            unreachable!()
                        };
                        let g_fail = format!("(not {c})");
                        let mut qf = qb.clone();
                        qf.assertions.push(g_fail.clone());
                        let mut qp = qb.clone();
                        qp.assertions.push(c.clone());
                        fail_qb = Some(qf);
                        pass_qb = Some(qp);
                        goal_pair = Some((g_fail, c.clone()));
                    }
                    ObligationKind::FoldBase { .. } => {
                        let n_ops = op.operands.len();
                        if let Operand::Invariant(v) = op.operands[n_ops - 1] {
                            let acc_ty = &op.results[0];
                            let init = &args[n_ops - 2];
                            let mut qf = qb.clone();
                            let mut ef = make_env(node, callee, inputs.clone(), vec![]);
                            ef.ports = ports.clone();
                            let neg = invariant_smt(
                                node,
                                v,
                                step_count("(_ bv0 65)".into(), 65),
                                symval_to_fval(init, acc_ty),
                                &mut ef,
                                &mut qf,
                                true,
                            );
                            qf.assertions.push(neg.clone());

                            let mut qp = qb.clone();
                            let mut ep = make_env(node, callee, inputs.clone(), vec![]);
                            ep.ports = ports.clone();
                            let hold = invariant_smt(
                                node,
                                v,
                                step_count("(_ bv0 65)".into(), 65),
                                symval_to_fval(init, acc_ty),
                                &mut ep,
                                &mut qp,
                                false,
                            );
                            qp.assertions.push(hold.clone());

                            fail_qb = Some(qf);
                            pass_qb = Some(qp);
                            goal_pair = Some((neg, hold));
                        }
                    }
                    ObligationKind::FoldPre { pre_idx, .. } => {
                        let n_ops = op.operands.len();
                        if let (Some(f), Operand::Invariant(v)) =
                            (callee_of(0), op.operands[n_ops - 1])
                            && let Some(t) = f.contract.pre.get(*pre_idx)
                        {
                            let acc_ty = &op.results[0];
                            let b = &args[2..n_ops - 2];
                            let mut qf = qb.clone();
                            let (k, x) = fresh_element(&args[1], &mut qf, "fold_k");
                            let acc = qf.fresh_symval(&format!("fold_acc{i}"), acc_ty);
                            let mut ef = make_env(node, callee, inputs.clone(), vec![]);
                            ef.ports = ports.clone();
                            let k65 = zext(&k, 64, 65);
                            let inv = invariant_smt(
                                node,
                                v,
                                step_count(k65, 65),
                                symval_to_fval(&acc, acc_ty),
                                &mut ef,
                                &mut qf,
                                false,
                            );
                            qf.assertions.push(inv);
                            let mut fa = vec![acc, x];
                            fa.extend(b.iter().cloned());
                            let mut fe = make_env(&f, callee, types_of(&f, &fa), vec![]);
                            let neg = encode_neg_goal(t, &mut fe, &mut qf);
                            qf.assertions.push(neg.clone());

                            // Passing input: unroll element 0 as a concrete baseline hint.
                            let mut qp = qb.clone();
                            let init = &args[n_ops - 2];
                            let idx0 = bv_lit_u128(0, 64);
                            let el0 = symval_select(&args[1], &idx0, &mut qp.next_coll_id);
                            qp.register_coll_id(&el0);
                            let mut ea = vec![init.clone(), el0];
                            ea.extend(b.iter().cloned());
                            let mut pe = make_env(&f, callee, types_of(&f, &ea), vec![]);
                            let hold = encode_assume(t, &mut pe, &mut qp);
                            qp.assertions.push(hold.clone());

                            fail_qb = Some(qf);
                            pass_qb = Some(qp);
                            goal_pair = Some((neg, hold));
                        }
                    }
                    ObligationKind::FoldStep { .. } => {
                        let n_ops = op.operands.len();
                        if let (Some(f), Operand::Invariant(v)) =
                            (callee_of(0), op.operands[n_ops - 1])
                        {
                            let acc_ty = op.results[0].clone();
                            let b = &args[2..n_ops - 2];
                            let mut qf = qb.clone();
                            let (k, x) = fresh_element(&args[1], &mut qf, "fold_k");
                            let acc = qf.fresh_symval(&format!("fold_acc{i}"), &acc_ty);
                            let mut ef = make_env(node, callee, inputs.clone(), vec![]);
                            ef.ports = ports.clone();
                            let k65 = zext(&k, 64, 65);
                            let inv = invariant_smt(
                                node,
                                v,
                                step_count(k65, 65),
                                symval_to_fval(&acc, &acc_ty),
                                &mut ef,
                                &mut qf,
                                false,
                            );
                            qf.assertions.push(inv);
                            let mut fa = vec![acc, x];
                            fa.extend(b.iter().cloned());
                            let r = qf.fresh_symval(&format!("fold_r{i}"), &acc_ty);
                            let mut fe = make_env(
                                &f,
                                callee,
                                types_of(&f, &fa),
                                vec![(acc_ty.clone(), r.clone())],
                            );
                            let post = assume_conjuncts(&f.contract.post, &mut fe, &mut qf);
                            qf.assertions.push(post);
                            let k66 = format!("(bvadd {} (_ bv1 66))", zext(&k, 64, 66));
                            let neg = invariant_smt(
                                node,
                                v,
                                step_count(k66, 66),
                                symval_to_fval(&r, &acc_ty),
                                &mut ef,
                                &mut qf,
                                true,
                            );
                            qf.assertions.push(neg.clone());

                            // Passing input: step 0 from init satisfies I(1, r0) if len > 0.
                            let mut qp = qb.clone();
                            let init = &args[n_ops - 2];
                            let idx0 = bv_lit_u128(0, 64);
                            let el0 = symval_select(&args[1], &idx0, &mut qp.next_coll_id);
                            qp.register_coll_id(&el0);
                            let mut ea = vec![init.clone(), el0];
                            ea.extend(b.iter().cloned());
                            let r0 = qp.fresh_symval(&format!("fold_r0_{i}"), &acc_ty);
                            let mut pe0 = make_env(
                                &f,
                                callee,
                                types_of(&f, &ea),
                                vec![(acc_ty.clone(), r0.clone())],
                            );
                            let post0 = assume_conjuncts(&f.contract.post, &mut pe0, &mut qp);
                            qp.assertions.push(post0);
                            let mut pe1 = make_env(node, callee, inputs.clone(), vec![]);
                            pe1.ports = ports.clone();
                            let hold = invariant_smt(
                                node,
                                v,
                                step_count("(_ bv1 65)".into(), 65),
                                symval_to_fval(&r0, &acc_ty),
                                &mut pe1,
                                &mut qp,
                                false,
                            );
                            qp.assertions.push(hold.clone());

                            fail_qb = Some(qf);
                            pass_qb = Some(qp);
                            goal_pair = Some((neg, hold));
                        }
                    }
                    _ => {}
                }
            }

            // Advance body symbolic state across op `i`.
            let results = if info.name == "FOLD_BOUNDED" {
                let n_ops = op.operands.len();
                if let Operand::Invariant(v) = op.operands[n_ops - 1] {
                    let acc_ty = op.results[0].clone();
                    let res = qb.fresh_symval(&format!("p{i}_fold"), &acc_ty);
                    let len65 = zext(&coll_len_bv64(&args[1]), 64, 65);
                    let mut e = make_env(node, callee, inputs.clone(), vec![]);
                    e.ports = ports.clone();
                    let fact = invariant_smt(
                        node,
                        v,
                        step_count(len65, 65),
                        symval_to_fval(&res, &acc_ty),
                        &mut e,
                        &mut qb,
                        false,
                    );
                    op_eqs
                        .entry(i_u32)
                        .or_default()
                        .push((symval_atoms(&res).into_iter().collect(), fact.clone()));
                    qb.assertions.push(fact);
                    vec![res]
                } else {
                    op.results
                        .iter()
                        .map(|t| qb.fresh_symval(&format!("p{i}"), t))
                        .collect()
                }
            } else {
                let prev_assertions = qb.assertions.len();
                let raw = eval_op_core(
                    node,
                    op,
                    &args,
                    &format!("p{i}"),
                    DomainMode::Strict,
                    callee,
                    &mut qb,
                    0,
                );
                qb.domains.clear();
                if qb.assertions.len() > prev_assertions {
                    for a in &qb.assertions[prev_assertions..] {
                        op_eqs
                            .entry(i_u32)
                            .or_default()
                            .push((BTreeSet::new(), a.clone()));
                    }
                }
                if name_scalar_ops {
                    let mut bound_res = Vec::with_capacity(raw.len());
                    for (slot, (sv, ty)) in raw.into_iter().zip(&op.results).enumerate() {
                        if is_scalar_or_bool_ty(ty) {
                            let sym = declare_symval(
                                &format!("p{i}_{slot}"),
                                ty,
                                &mut qb.decls,
                                &mut qb.next_coll_id,
                            );
                            let sym_atoms = symval_atoms(&sym);
                            let rhs_atoms = symval_atoms(&sv);
                            if sym_atoms.len() == 1 && rhs_atoms.len() == 1 {
                                let eq = format!("(= {} {})", sym_atoms[0], rhs_atoms[0]);
                                let mut defined = BTreeSet::new();
                                defined.insert(sym_atoms[0].clone());
                                qb.add_pure_def(defined.clone(), eq.clone());
                                op_eqs.entry(i_u32).or_default().push((defined, eq));
                                bound_res.push(sym);
                                continue;
                            }
                        }
                        bound_res.push(sv);
                    }
                    bound_res
                } else {
                    raw
                }
            };

            for (s, sv) in results.iter().enumerate() {
                ports.insert((i_u32, s as u8), (op.results[s].clone(), sv.clone()));
            }
            vals.push(results);
        }

        let results: Vec<(Type, SymVal)> = node
            .outputs
            .iter()
            .zip(&node.results)
            .map(|(o, t)| {
                (
                    t.clone(),
                    operand_sym(node, *o, &inputs, &vals, &mut consts, &mut qb),
                )
            })
            .collect();

        match kind {
            ObligationKind::Post(j) => {
                if let Some(t) = node.contract.post.get(*j) {
                    let mut qf = qb.clone();
                    let mut ef = make_env(node, callee, inputs.clone(), results.clone());
                    ef.ports = ports.clone();
                    let neg = encode_neg_goal(t, &mut ef, &mut qf);
                    qf.assertions.push(neg.clone());

                    let mut qp = qb.clone();
                    let mut ep = make_env(node, callee, inputs.clone(), results);
                    ep.ports = ports.clone();
                    let hold = encode_assume(t, &mut ep, &mut qp);
                    qp.assertions.push(hold.clone());

                    fail_qb = Some(qf);
                    pass_qb = Some(qp);
                    goal_pair = Some((neg, hold));
                }
            }
            ObligationKind::PreWd(j, w) => {
                let mut ctx = pre_base;
                for prev in 0..*j {
                    if let Some(t) = node.contract.pre.get(prev) {
                        let mut e = make_env(node, callee, inputs.clone(), vec![]);
                        let a = encode_assume(t, &mut e, &mut ctx);
                        ctx.assertions.push(a);
                    }
                }
                if let Some(t) = node.contract.pre.get(*j)
                    && let Some(wd) = collect_wd_obligations(t, node, callee).get(*w)
                {
                    let mut qf = ctx.clone();
                    let mut e = make_env(node, callee, inputs.clone(), vec![]);
                    let neg = encode_wd_obligation(wd, &mut e, &mut qf);
                    qf.assertions.push(neg.clone());
                    fail_qb = Some(qf);
                    goal_pair = Some((neg, "true".into()));
                }
            }
            ObligationKind::PostWd(j, w) => {
                if let Some(t) = node.contract.post.get(*j)
                    && let Some(wd) = collect_wd_obligations(t, node, callee).get(*w)
                {
                    let mut qf = qb.clone();
                    let mut e = make_env(node, callee, inputs.clone(), results);
                    e.ports = ports.clone();
                    let neg = encode_wd_obligation(wd, &mut e, &mut qf);
                    qf.assertions.push(neg.clone());
                    fail_qb = Some(qf);
                    goal_pair = Some((neg, "true".into()));
                }
            }
            ObligationKind::NonVacuous => {
                fail_qb = Some(pre_qb.clone());
                pass_qb = Some(pre_qb.clone());
            }
            _ => {}
        }

        Self {
            inputs,
            pre_qb,
            body_qb: qb,
            op_syms: vals,
            op_eqs,
            fail_qb,
            pass_qb,
            goal_pair,
        }
    }

    /// Computes the canonical 32-byte obligation hash (`VeriGraph 2026 obligation v2`).
    pub fn obligation_hash(
        &self,
        node: &Node,
        obligation_id: &str,
        callee: &dyn Fn(&Hash) -> Option<Node>,
    ) -> Hash {
        if let Some(qf) = &self.fail_qb {
            let mut q = qf.clone();
            q.finalize_instantiations(callee);
            let smt = q.into_pruned_query().to_smtlib2(0);
            return derive(Context::Obligation, smt.as_bytes());
        }
        // Fallback for obligations without SMT encoding (e.g. COST): deterministic hash over node + id.
        let (bytes, _) = vg_core::codec::encode(node);
        let mut data = Vec::with_capacity(bytes.len() + 1 + obligation_id.len());
        data.extend_from_slice(&bytes);
        data.push(0);
        data.extend_from_slice(obligation_id.as_bytes());
        derive(Context::Obligation, &data)
    }

    /// Searches for a passing input vector via Z3 solving `PRE /\ body /\ goal_holds`.
    /// Tries single-input perturbations around `inputs_fail` first so unrelated inputs keep
    /// their counterexample values, then falls back to unconstrained solving.
    pub fn find_passing_inputs(
        &self,
        node: &Node,
        kind: &ObligationKind,
        inputs_fail: &[Value],
        callee: &dyn Fn(&Hash) -> Option<Node>,
        timeout_ms: u64,
    ) -> Option<Vec<Value>> {
        let base_pass = self.pass_qb.as_ref()?;
        let mut q_base = base_pass.clone();
        q_base.finalize_instantiations(callee);
        if q_base.unresolved_error.is_some() {
            return None;
        }

        let z3_path = vg_smt::find_z3();
        // A wall-clock limit made the slice depend on the machine: CI's runner most likely ran out
        // of time (1.5 s; the query takes 0.66 s here), and the seeded fallback gives exactly the
        // 6-op slice CI reported for the soft knee. z3's resource limit stops at the same point
        // everywhere; 20M units is about 4 s on an M-series Mac, and the suites' passing-input
        // queries need at most 4.8M. The clock stays only as a guard against a hang.
        let mut z3 = Z3Session::new(&z3_path, timeout_ms).with_rlimit(20_000_000);
        let input_syms: Vec<SymVal> = self.inputs.iter().map(|(_, s)| s.clone()).collect();

        // Try pinning all scalar inputs except one `m` to their counterexample values first.
        if node.input_count() >= 2 && node.input_count() <= 6 {
            for m in 0..node.input_count() {
                let mut q = q_base.clone();
                let mut pinned = 0usize;
                for (j, (ty, sym)) in self.inputs.iter().enumerate() {
                    if j == m {
                        continue;
                    }
                    if let Some(eq) = scalar_pin_smt(ty, sym, &inputs_fail[j]) {
                        q.assertions.push(eq);
                        pinned += 1;
                    }
                }
                if pinned == 0 {
                    continue;
                }
                let smt_q = q.into_pruned_query();
                if matches!(z3.check_query(&smt_q), CheckResult::Sat)
                    && let DecodedModel::Values(vals) =
                        decode_with_small_model(&mut z3, &smt_q, node, &input_syms)
                {
                    let tr = trace_eval(node, &vals, callee);
                    let chk = check_violation(node, kind, &vals, &tr, callee);
                    if chk.pre_holds && !chk.violated {
                        return Some(vals);
                    }
                }
            }
        }

        // Fallback: solve unconstrained `PRE /\ body /\ goal_holds`.
        let smt_q = q_base.into_pruned_query();
        if matches!(z3.check_query(&smt_q), CheckResult::Sat)
            && let DecodedModel::Values(vals) =
                decode_with_small_model(&mut z3, &smt_q, node, &input_syms)
        {
            let tr = trace_eval(node, &vals, callee);
            let chk = check_violation(node, kind, &vals, &tr, callee);
            if chk.pre_holds && !chk.violated {
                return Some(vals);
            }
        }

        None
    }
}

fn scalar_pin_smt(ty: &Type, sym: &SymVal, val: &Value) -> Option<String> {
    match (ty, sym, val) {
        (Type::Bool, SymVal::Bool(e), Value::Bool(b)) => Some(format!("(= {e} {b})")),
        (Type::UInt(w) | Type::SInt(w), SymVal::Int { expr, .. }, Value::Int(v)) => {
            Some(format!("(= {expr} {})", bv_lit_i128(*v, u32::from(*w))))
        }
        (t, SymVal::Float { expr, .. }, Value::Float(bits)) if t.is_float() => {
            let mut buf = [0u8; 8];
            let sz = t.byte_size()? as usize;
            buf[..sz].copy_from_slice(&bits.to_le_bytes()[..sz]);
            let lit = scalar_const_smt(t, &buf[..sz]);
            Some(format!("(= {expr} {lit})"))
        }
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct Candidate {
    smt: String,
    text: String,
}

/// Computes the weakest constraint diff on `slice_ops` via Z3 unsat cores.
/// Returns `None` if no candidate yields a genuine unsat core containing `"cand"`.
pub fn diff(
    node: &Node,
    obligation: &str,
    names: &Names,
    slice_ops: &[u32],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Option<String> {
    let kind = parse_obligation_id(obligation)?;
    if matches!(
        kind,
        ObligationKind::NonVacuous
            | ObligationKind::Cost(_)
            | ObligationKind::PreWd(..)
            | ObligationKind::PostWd(..)
    ) {
        return None;
    }

    let ctx = SmtObligationContext::build(node, &kind, callee, true);
    let (goal_fails, goal_holds) = ctx.goal_pair.clone()?;

    // Finalize instantiations on a clone of `fail_qb` so all needed axioms/decls are materialized,
    // while preserving named slice equations.
    let mut base_qb = ctx.fail_qb.clone()?;
    base_qb.finalize_instantiations(callee);
    if base_qb.unresolved_error.is_some() {
        return None;
    }

    // Remove the raw unnamed `goal_fails` at the end of `base_qb.assertions` (if present) so we
    // can assert it with `:named goal`.
    if base_qb.assertions.last() == Some(&goal_fails) {
        base_qb.assertions.pop();
    }

    let slice_set: BTreeSet<u32> = slice_ops.iter().copied().collect();
    let mut named_eq_strings: BTreeSet<String> = BTreeSet::new();
    let mut slice_named: Vec<(String, String)> = Vec::new();
    for &op_idx in slice_ops {
        if let Some(eqs) = ctx.op_eqs.get(&op_idx) {
            for (k, (_, eq)) in eqs.iter().enumerate() {
                named_eq_strings.insert(eq.clone());
                slice_named.push((format!("e{op_idx}_{k}"), eq.clone()));
            }
        }
    }

    let candidates = generate_candidates(node, &kind, names, slice_ops, &ctx);
    if candidates.is_empty() {
        return None;
    }

    let z3_path = vg_smt::find_z3();
    let mut uc_sess = UnsatCoreSession::new(&z3_path, 3000);
    let mut nec_sess = Z3Session::new(&z3_path, 2500);

    // Build a pruned template query containing `base_qb` plus all slice equations.
    let mut template_qb = base_qb.clone();
    // Ensure slice symbols are kept during pruning.
    for &op_idx in slice_ops {
        if let Some(syms) = ctx.op_syms.get(op_idx as usize) {
            for sv in syms {
                template_qb.always_keep_decls.extend(symval_atoms(sv));
            }
        }
    }
    template_qb.assertions.push(goal_fails.clone());
    let pruned = template_qb.into_pruned_query();
    let use_qffp = pruned.decls.values().any(|s| s.contains("FloatingPoint"));

    // Separate background assertions from named slice equations and `goal_fails`.
    let mut bg_assertions = Vec::new();
    for a in &pruned.assertions {
        if *a == goal_fails || named_eq_strings.contains(a) {
            continue;
        }
        bg_assertions.push(a.clone());
    }
    // Note: if any op_idx in slice_set had pure_defs pruned or kept, ensure every slice_named whose
    // symbols are declared in `pruned.decls` is asserted as `(! eq :named eK)`.
    let mut active_slice_named = Vec::new();
    for (tag, eq) in &slice_named {
        if let Some(op_str) = tag.strip_prefix('e').and_then(|s| s.split('_').next())
            && let Ok(op_idx) = op_str.parse::<u32>()
            && slice_set.contains(&op_idx)
        {
            active_slice_named.push((tag.clone(), eq.clone()));
        }
    }

    for cand in candidates.into_iter().take(12) {
        let mut script = String::with_capacity(4096);
        script.push_str("(reset)\n");
        script.push_str("(set-option :produce-unsat-cores true)\n");
        script.push_str("(set-option :timeout 3000)\n");
        script.push_str("(set-option :smt.random_seed 1)\n");
        script.push_str("(set-option :sat.random_seed 1)\n");
        script.push_str("(set-logic QF_AUFBVFP)\n");
        for (name, sort) in &pruned.decls {
            script.push_str(&format!("(declare-const {name} {sort})\n"));
        }
        for (name, (args, ret)) in &pruned.fun_decls {
            script.push_str(&format!(
                "(declare-fun {name} ({}) {ret})\n",
                args.join(" ")
            ));
        }
        for (idx, bg) in bg_assertions.iter().enumerate() {
            script.push_str(&format!("(assert (! {bg} :named bg{idx}))\n"));
        }
        for (tag, eq) in &active_slice_named {
            script.push_str(&format!("(assert (! {eq} :named {tag}))\n"));
        }
        script.push_str(&format!("(assert (! {goal_fails} :named goal))\n"));
        script.push_str(&format!("(assert (! {} :named cand))\n", cand.smt));
        if use_qffp {
            script.push_str("(check-sat-using qffp)\n");
        } else {
            script.push_str("(check-sat)\n");
        }

        if uc_sess.check_unsat_core(&script) == Some(true) {
            // Check necessity: is `PRE /\ body /\ (not cand) /\ goal_holds` UNSAT?
            let mut nec_qb = ctx.body_qb.clone();
            nec_qb.assertions.push(format!("(not {})", cand.smt));
            nec_qb.assertions.push(goal_holds.clone());
            nec_qb.finalize_instantiations(callee);
            let nec_q = nec_qb.into_pruned_query();
            let is_iff = matches!(nec_sess.check_query(&nec_q), CheckResult::Unsat);
            let conn = if is_iff { "iff" } else { "if" };
            return Some(format!("the obligation holds {conn} {}", cand.text));
        }
    }

    None
}

fn generate_candidates(
    node: &Node,
    kind: &ObligationKind,
    names: &Names,
    slice_ops: &[u32],
    ctx: &SmtObligationContext,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut seen_smt = BTreeSet::new();
    let mut push = |smt: String, text: String| {
        if seen_smt.insert(smt.clone()) {
            out.push(Candidate { smt, text });
        }
    };

    // Extract integer constants appearing in the node / post clause for bounded comparisons.
    let mut int_bounds: BTreeSet<i128> = BTreeSet::new();
    for c in &node.consts {
        if c.ty == Type::Int
            && let Some(v) = vg_core::lower::int_value(&c.bytes)
        {
            int_bounds.insert(v);
        } else if c.ty.is_int()
            && let Some((Value::Int(v), _)) = Value::from_bytes(&c.bytes, &c.ty)
        {
            int_bounds.insert(v);
        }
    }
    if let ObligationKind::Post(j) = kind
        && let Some(t) = node.contract.post.get(*j)
    {
        collect_term_int_lits(node, t, &mut int_bounds);
    }

    // First, op-specific operand domain candidates if this is an op obligation (when useful).
    if let Some(op_idx) = kind.op_index()
        && let Some(op) = node.ops.get(op_idx as usize)
    {
        let info = opcode::by_code(op.opcode);
        match info.map(|x| x.name) {
            Some("DIV" | "MOD") => {
                if let Some(Operand::Op(src_i, src_s)) = op.operands.get(1)
                    && let Some(SymVal::Int { expr, ty }) = ctx
                        .op_syms
                        .get(*src_i as usize)
                        .and_then(|v| v.get(*src_s as usize))
                    && let Some(w) = ty.int_bits()
                {
                    let pname = names.port_name(*src_i, *src_s);
                    push(
                        format!("(not (= {expr} {}))", bv_lit_u128(0, u32::from(w))),
                        format!("{pname} != 0"),
                    );
                }
            }
            Some("INDEX" | "UPDATE") => {
                if let Some(Operand::Op(idx_i, idx_s)) = op.operands.get(1)
                    && let Some(SymVal::Int { expr, .. }) = ctx
                        .op_syms
                        .get(*idx_i as usize)
                        .and_then(|v| v.get(*idx_s as usize))
                {
                    let xs_sym = match op.operands[0] {
                        Operand::Input(k) => Some(&ctx.inputs[k as usize].1),
                        Operand::Op(k, s) => {
                            ctx.op_syms.get(k as usize).and_then(|v| v.get(s as usize))
                        }
                        _ => None,
                    };
                    if let Some(xs) = xs_sym {
                        let len_bv = coll_len_bv64(xs);
                        let pname = names.port_name(*idx_i, *idx_s);
                        let xs_name = names.format_operand(node, op.operands[0]);
                        push(
                            format!("(bvult {expr} {len_bv})"),
                            format!("{pname} < len({xs_name})"),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    // Per-slice-op candidates in canonical order (earliest op first, weakest predicate first).
    let op_count = slice_ops.len();
    for (pos, &op_idx) in slice_ops.iter().enumerate() {
        let Some(op) = node.ops.get(op_idx as usize) else {
            continue;
        };
        let Some(syms) = ctx.op_syms.get(op_idx as usize) else {
            continue;
        };
        let is_last_post_op =
            matches!(kind, ObligationKind::Post(_)) && op_count > 1 && pos + 1 == op_count;
        if is_last_post_op {
            // Prefer explaining the root cause via earlier slice ops before trying predicates on
            // the final output op itself.
            continue;
        }
        for (slot, (sv, ty)) in syms.iter().zip(&op.results).enumerate() {
            if *ty == Type::Eff {
                continue;
            }
            let pname = names.port_name(op_idx, slot as u8);
            match sv {
                SymVal::Float { expr, .. } if ty.is_float() => {
                    if let Some((eb, sb)) = fp_format(ty) {
                        push(
                            format!("(not (fp.isNaN {expr}))"),
                            format!("{pname} is not NaN"),
                        );
                        push(
                            format!("(and (not (fp.isNaN {expr})) (not (fp.isInfinite {expr})))"),
                            format!("{pname} is finite"),
                        );
                        push(
                            format!("(not (fp.isZero {expr}))"),
                            format!("{pname} != 0.0"),
                        );
                        push(
                            format!("(fp.geq {expr} (_ +zero {eb} {sb}))"),
                            format!("{pname} >= 0.0"),
                        );
                        push(
                            format!("(fp.gt {expr} (_ +zero {eb} {sb}))"),
                            format!("{pname} > 0.0"),
                        );
                    }
                }
                SymVal::Int { expr, .. } => {
                    if let Some(w) = ty.int_bits() {
                        let w32 = u32::from(w);
                        let zero = bv_lit_u128(0, w32);
                        push(format!("(not (= {expr} {zero}))"), format!("{pname} != 0"));
                        if matches!(ty, Type::SInt(_)) {
                            push(format!("(bvsge {expr} {zero})"), format!("{pname} >= 0"));
                            push(format!("(bvsgt {expr} {zero})",), format!("{pname} > 0"));
                        } else {
                            push(format!("(bvugt {expr} {zero})"), format!("{pname} > 0"));
                        }
                        for &b in int_bounds.iter().take(4) {
                            if vg_core::value::int_fits(b, ty) && b != 0 {
                                let blit = bv_lit_i128(b, w32);
                                if matches!(ty, Type::SInt(_)) {
                                    push(
                                        format!("(bvsle {expr} {blit})"),
                                        format!("{pname} <= {b}"),
                                    );
                                    push(
                                        format!("(bvslt {expr} {blit})"),
                                        format!("{pname} < {b}"),
                                    );
                                    push(
                                        format!("(bvsge {expr} {blit})"),
                                        format!("{pname} >= {b}"),
                                    );
                                } else {
                                    push(
                                        format!("(bvule {expr} {blit})"),
                                        format!("{pname} <= {b}"),
                                    );
                                    push(
                                        format!("(bvult {expr} {blit})"),
                                        format!("{pname} < {b}"),
                                    );
                                    push(
                                        format!("(bvuge {expr} {blit})"),
                                        format!("{pname} >= {b}"),
                                    );
                                }
                            }
                        }
                    }
                }
                SymVal::Bool(expr) => {
                    push(expr.clone(), format!("{pname} is true"));
                    push(format!("(not {expr})"), format!("{pname} is false"));
                }
                SymVal::Result { is_ok, .. } => {
                    push(is_ok.clone(), format!("is_ok({pname})"));
                    push(format!("(not {is_ok})"), format!("!is_ok({pname})"));
                }
                _ => {}
            }
        }
    }

    // If no earlier op yielded a candidate (e.g. single-op slice), also add input-level candidates
    // or final-op candidates as fallback.
    if let Some(&last_op) = slice_ops.last()
        && op_count > 1
        && let (Some(op), Some(syms)) = (
            node.ops.get(last_op as usize),
            ctx.op_syms.get(last_op as usize),
        )
    {
        for (slot, (sv, ty)) in syms.iter().zip(&op.results).enumerate() {
            if *ty == Type::Eff {
                continue;
            }
            let pname = names.port_name(last_op, slot as u8);
            if let SymVal::Float { expr, .. } = sv {
                push(
                    format!("(not (fp.isNaN {expr}))"),
                    format!("{pname} is not NaN"),
                );
                push(
                    format!("(and (not (fp.isNaN {expr})) (not (fp.isInfinite {expr})))"),
                    format!("{pname} is finite"),
                );
            }
        }
    }

    // Also allow scalar input predicates when slice_ops is small (e.g., 1 op directly on inputs).
    if slice_ops.len() <= 2 {
        for (i, (ty, sv)) in ctx.inputs.iter().enumerate() {
            let iname = names.input_name(i as u32);
            match sv {
                SymVal::Int { expr, .. } if ty.int_bits().is_some() => {
                    let w32 = u32::from(ty.int_bits().unwrap());
                    let zero = bv_lit_u128(0, w32);
                    push(format!("(not (= {expr} {zero}))"), format!("{iname} != 0"));
                    if matches!(ty, Type::SInt(_)) {
                        push(format!("(bvsge {expr} {zero})"), format!("{iname} >= 0"));
                    }
                }
                SymVal::Float { expr, .. } => {
                    push(
                        format!("(not (fp.isNaN {expr}))"),
                        format!("{iname} is not NaN"),
                    );
                    push(
                        format!("(and (not (fp.isNaN {expr})) (not (fp.isInfinite {expr})))"),
                        format!("{iname} is finite"),
                    );
                    push(
                        format!("(not (fp.isZero {expr}))"),
                        format!("{iname} != 0.0"),
                    );
                }
                _ => {}
            }
        }
    }

    out
}

fn collect_term_int_lits(node: &Node, t: &Term, out: &mut BTreeSet<i128>) {
    if let Term::Const(c) = t
        && let Some(kc) = node.consts.get(*c as usize)
    {
        if kc.ty == Type::Int
            && let Some(v) = vg_core::lower::int_value(&kc.bytes)
        {
            out.insert(v);
        } else if kc.ty.is_int()
            && let Some((Value::Int(v), _)) = Value::from_bytes(&kc.bytes, &kc.ty)
        {
            out.insert(v);
        }
    }
    for c in vg_core::canon::children(t) {
        collect_term_int_lits(node, c, out);
    }
}

struct UnsatCoreSession {
    z3_path: PathBuf,
    timeout_ms: u64,
    proc: Option<LiveZ3>,
}

struct LiveZ3 {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl UnsatCoreSession {
    fn new(z3_path: &Path, timeout_ms: u64) -> Self {
        Self {
            z3_path: z3_path.to_path_buf(),
            timeout_ms,
            proc: None,
        }
    }

    fn ensure_started(&mut self) -> Result<&mut LiveZ3, String> {
        if self.proc.is_none() {
            let mut child = Command::new(&self.z3_path)
                .arg("-in")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("failed to start z3: {e}"))?;
            let stdin = BufWriter::new(child.stdin.take().ok_or("missing stdin")?);
            let stdout = BufReader::new(child.stdout.take().ok_or("missing stdout")?);
            self.proc = Some(LiveZ3 {
                child,
                stdin,
                stdout,
            });
        }
        Ok(self.proc.as_mut().expect("started"))
    }

    fn kill_proc(&mut self) {
        if let Some(mut p) = self.proc.take() {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }

    /// Runs `script`, checks for `unsat`, then calls `(get-unsat-core)` and returns `Some(true)`
    /// iff `"cand"` is present in the unsat core.
    fn check_unsat_core(&mut self, script: &str) -> Option<bool> {
        let _ = self.timeout_ms;
        let expr = {
            let proc = self.ensure_started().ok()?;
            if proc
                .stdin
                .write_all(script.as_bytes())
                .and_then(|()| proc.stdin.flush())
                .is_err()
            {
                self.kill_proc();
                return None;
            }
            match read_sexpr(&mut proc.stdout) {
                Ok(Some(e)) => e,
                _ => {
                    self.kill_proc();
                    return None;
                }
            }
        };

        if !matches!(&expr, SExpr::Atom(a) if a == "unsat") {
            return Some(false);
        }

        let core_expr = {
            let proc = self.proc.as_mut()?;
            if proc
                .stdin
                .write_all(b"(get-unsat-core)\n")
                .and_then(|()| proc.stdin.flush())
                .is_err()
            {
                self.kill_proc();
                return None;
            }
            match read_sexpr(&mut proc.stdout) {
                Ok(Some(e)) => e,
                _ => {
                    self.kill_proc();
                    return None;
                }
            }
        };

        if let SExpr::List(items) = core_expr {
            let has_cand = items.iter().any(|x| x.as_atom() == Some("cand"));
            Some(has_cand)
        } else {
            None
        }
    }
}

impl Drop for UnsatCoreSession {
    fn drop(&mut self) {
        if let Some(mut p) = self.proc.take() {
            let _ = p.stdin.write_all(b"(exit)\n");
            let _ = p.stdin.flush();
            let _ = p.child.wait();
        }
    }
}

trait TypeIntBits {
    fn int_bits(&self) -> Option<u8>;
}

impl TypeIntBits for Type {
    fn int_bits(&self) -> Option<u8> {
        match self {
            Type::UInt(w) | Type::SInt(w) => Some(*w),
            _ => None,
        }
    }
}
