//! The obligation layer and the driver of `verify` (P3.2, P3.4, P3.7): every obligation of docs/05
//! and docs/14 for one node, each solved as `context ∧ ¬goal`, named as docs/15 says.
//!
//! Soundness rules (docs/05; PROGRESS.md, 2026-11-06): an op's obligations see only the facts of
//! the ops before it in canonical order, never its own or later ones; UNSAT is the only proof, and
//! anything not encoded (a missing callee, a COST conjunct) is Unknown, never Proven.

use std::collections::BTreeMap;

use vg_core::ir::{Hash, Node, Operand};
use vg_core::opcode;
use vg_core::types::Type;

use crate::formula::{
    DomainMode, FVal, FormulaEnv, QueryBuilder, assume_conjuncts, coll_len_bv64,
    collect_wd_obligations, encode_assume, encode_neg_goal, encode_wd_obligation, eval_op_core,
    symval_to_fval, zext,
};
use crate::model::{DecodedModel, decode_inputs};
use crate::solver::{CheckResult, Solver, Z3Session};
use crate::sort::{
    SymVal, const_to_symval, declare_symval, scalar_const_smt, symval_atoms, symval_select,
};
use crate::{Options, Outcome, ReplayVerdict, Verdict, classify_sat_model};

/// One proof goal: its context with the negated goal already asserted.
struct Ob {
    id: String,
    qb: QueryBuilder,
    /// The NONVACUOUS check: SAT is the good answer.
    nonvacuous: bool,
}

/// The op kind docs/14 gives each domain that eval_op_core records.
fn kind(op: &str, tag: &str) -> String {
    let k = match (op, tag) {
        ("ADD" | "SUB" | "MUL" | "NEG", _) => "no_overflow",
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
    format!("{op}.{k}")
}

/// The symbolic value of an operand; callee and invariant operands get a placeholder, as
/// eval_op_core expects one argument per operand.
fn operand(
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

/// The context of `qb` with the definitions (never the facts) that evaluating op i added to
/// `scratch`, so a domain written over op i's operands is fully declared.
fn with_defs_of(qb: &QueryBuilder, scratch: &QueryBuilder) -> QueryBuilder {
    let mut q = qb.clone();
    q.decls.extend(scratch.decls.clone());
    q.fun_decls.extend(scratch.fun_decls.clone());
    q.pure_defs = scratch.pure_defs.clone();
    q.next_fresh = scratch.next_fresh;
    q.next_coll_id = scratch.next_coll_id;
    q
}

fn env<'a>(
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

/// `goal_i` for each conjunct of `pre` of `target` applied to `args`, under `cond`, as obligations.
#[allow(clippy::too_many_arguments)]
fn pre_obligations(
    id: &str,
    target: &Node,
    args: Vec<(Type, SymVal)>,
    cond: Option<&str>,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    qb: &QueryBuilder,
    out: &mut Vec<Ob>,
) {
    for (j, t) in target.contract.pre.iter().enumerate() {
        let mut q = qb.clone();
        let mut e = env(target, callee, args.clone(), vec![]);
        let neg = encode_neg_goal(t, &mut e, &mut q);
        q.assertions.push(match cond {
            Some(c) => format!("(and {c} {neg})"),
            None => neg,
        });
        out.push(Ob {
            id: format!("{id}.{j}"),
            qb: q,
            nonvacuous: false,
        });
    }
}

/// The index tuple of flat index `k` in a row-major tensor with these dimensions.
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

/// A fresh element of collection `xs`: its fresh index (a 64-bit symbol below the length) and its
/// value, recorded so assumed foralls over xs are instantiated at it (the fresh-element rule).
fn fresh_element(xs: &SymVal, elem_ty: &Type, q: &mut QueryBuilder, tag: &str) -> (String, SymVal) {
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
    let _ = elem_ty;
    (k, x)
}

/// I(k, acc) for INVARIANT v: Bound(1) is the step count, Bound(0) the accumulator.
fn invariant(
    node: &Node,
    v: u32,
    k: FVal,
    acc: FVal,
    e: &mut FormulaEnv<'_>,
    q: &mut QueryBuilder,
    goal: bool,
) -> String {
    let saved = std::mem::replace(&mut e.bound, vec![k, acc]);
    let terms = &node.contract.invariants[v as usize];
    let s = if goal {
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

/// Every obligation of one node, then the solver's verdict on each.
pub fn verify(
    node: &Node,
    label: &str,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    opts: &Options,
) -> Vec<Outcome> {
    let label = label.trim_start_matches('@');
    let mut obs: Vec<Ob> = Vec::new();
    let mut qb = QueryBuilder::default();

    // Inputs: parameters, then θ inside its LEARNABLE box.
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
        let SymVal::Float { expr, .. } = &inputs[node.params.len() + t].1 else {
            unreachable!("LEARNABLE parameters are floats")
        };
        let lo = scalar_const_smt(&l.ty, &node.consts[l.lo as usize].bytes);
        let hi = scalar_const_smt(&l.ty, &node.consts[l.hi as usize].bytes);
        qb.assertions
            .push(format!("(and (fp.leq {lo} {expr}) (fp.leq {expr} {hi}))"));
    }
    let pre_base = qb.clone();
    {
        let mut e = env(node, callee, inputs.clone(), vec![]);
        for t in &node.contract.pre {
            let a = encode_assume(t, &mut e, &mut qb);
            qb.assertions.push(a);
        }
    }
    obs.push(Ob {
        id: format!("@{label}/NONVACUOUS"),
        qb: qb.clone(),
        nonvacuous: true,
    });

    // Ops in canonical order: obligations first, against the context of earlier ops only.
    let mut vals: Vec<Vec<SymVal>> = Vec::new();
    let mut consts = BTreeMap::new();
    let mut ports = BTreeMap::new();
    for (i, op) in node.ops.iter().enumerate() {
        let info = opcode::by_code(op.opcode).expect("validated opcode");
        let at = format!("@{label}/%{i}");
        let args: Vec<SymVal> = op
            .operands
            .iter()
            .map(|o| operand(node, *o, &inputs, &vals, &mut consts, &mut qb))
            .collect();

        // The op's own domains: evaluate it on a scratch copy and keep only what it recorded.
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
        for (tag, dom) in &scratch.domains {
            let mut q = with_defs_of(&qb, &scratch);
            q.assertions.push(format!("(not {dom})"));
            obs.push(Ob {
                id: format!("{at}/{}", kind(info.name, tag)),
                qb: q,
                nonvacuous: false,
            });
        }

        let callee_of = |j: usize| match op.operands[j] {
            Operand::Callee(c) => callee(&node.calls[c as usize]),
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
        let mut fold_result = None;
        match info.name {
            "CALL_HASH" => match callee_of(0) {
                Some(f) => pre_obligations(
                    &format!("{at}/CALL.pre"),
                    &f,
                    types_of(&f, &args[1..]),
                    None,
                    callee,
                    &qb,
                    &mut obs,
                ),
                None => {
                    qb.unresolved_error
                        .get_or_insert_with(|| format!("{at}: callee not found"));
                }
            },
            "ASSERT_GUARD" => {
                let SymVal::Bool(c) = &args[0] else {
                    unreachable!("Bool condition")
                };
                let last = op.operands.len() - 1;
                let b = &args[2..last];
                if let (Some(f), Some(g)) = (callee_of(1), callee_of(last)) {
                    pre_obligations(
                        &format!("{at}/GUARD.then.pre"),
                        &f,
                        types_of(&f, b),
                        Some(c),
                        callee,
                        &qb,
                        &mut obs,
                    );
                    let not_c = format!("(not {c})");
                    pre_obligations(
                        &format!("{at}/GUARD.else.pre"),
                        &g,
                        types_of(&g, b),
                        Some(&not_c),
                        callee,
                        &qb,
                        &mut obs,
                    );
                }
            }
            "SIMD_MAP" => {
                if let Some(f) = callee_of(0) {
                    let mut q = qb.clone();
                    let elem_ty = node
                        .operand_type(op.operands[1])
                        .and_then(Type::elem)
                        .expect("collection")
                        .clone();
                    let (_, x) = fresh_element(&args[1], &elem_ty, &mut q, "map_k");
                    let mut a = vec![x];
                    a.extend(args[2..].iter().cloned());
                    pre_obligations(
                        &format!("{at}/MAP.pre"),
                        &f,
                        types_of(&f, &a),
                        None,
                        callee,
                        &q,
                        &mut obs,
                    );
                }
            }
            "GENERATE" => {
                if let Some(f) = callee_of(0) {
                    let mut q = qb.clone();
                    let SymVal::Int { expr: n, .. } = &args[1] else {
                        unreachable!("u64 count")
                    };
                    let k = q.fresh_sym("gen_k", "(_ BitVec 64)");
                    q.assertions.push(format!("(bvult {k} {n})"));
                    let mut a = vec![SymVal::Int {
                        expr: k,
                        ty: Type::UInt(64),
                    }];
                    a.extend(args[2..].iter().cloned());
                    pre_obligations(
                        &format!("{at}/GEN.pre"),
                        &f,
                        types_of(&f, &a),
                        None,
                        callee,
                        &q,
                        &mut obs,
                    );
                }
            }
            "FOLD_BOUNDED" => {
                let n_ops = op.operands.len();
                let (Some(f), Operand::Invariant(v)) = (callee_of(0), op.operands[n_ops - 1])
                else {
                    qb.unresolved_error
                        .get_or_insert_with(|| format!("{at}: FOLD_BOUNDED kernel not found"));
                    vals.push(
                        op.results
                            .iter()
                            .map(|t| qb.fresh_symval(&format!("p{i}"), t))
                            .collect(),
                    );
                    continue;
                };
                let acc_ty = op.results[0].clone();
                let init = &args[n_ops - 2];
                let b = &args[2..n_ops - 2];
                let elem_ty = node
                    .operand_type(op.operands[1])
                    .and_then(Type::elem)
                    .expect("collection")
                    .clone();
                let fold_env = |q: &QueryBuilder| {
                    let _ = q;
                    let mut e = env(node, callee, inputs.clone(), vec![]);
                    e.ports = ports.clone();
                    e
                };
                // FOLD.base: I(0, init).
                let mut q = qb.clone();
                let mut e = fold_env(&q);
                let neg = invariant(
                    node,
                    v,
                    step_count("(_ bv0 65)".into(), 65),
                    symval_to_fval(init, &acc_ty),
                    &mut e,
                    &mut q,
                    true,
                );
                q.assertions.push(neg);
                obs.push(Ob {
                    id: format!("{at}/FOLD.base"),
                    qb: q,
                    nonvacuous: false,
                });
                // FOLD.pre.j and FOLD.step, for a fresh k < len(xs) and a fresh acc with I(k, acc).
                let mut q = qb.clone();
                let (k, x) = fresh_element(&args[1], &elem_ty, &mut q, "fold_k");
                let acc = q.fresh_symval(&format!("fold_acc{i}"), &acc_ty);
                let mut e = fold_env(&q);
                let k65 = zext(&k, 64, 65);
                let inv = invariant(
                    node,
                    v,
                    step_count(k65.clone(), 65),
                    symval_to_fval(&acc, &acc_ty),
                    &mut e,
                    &mut q,
                    false,
                );
                q.assertions.push(inv);
                let mut fa = vec![acc.clone(), x];
                fa.extend(b.iter().cloned());
                pre_obligations(
                    &format!("{at}/FOLD.pre"),
                    &f,
                    types_of(&f, &fa),
                    None,
                    callee,
                    &q,
                    &mut obs,
                );
                let r = q.fresh_symval(&format!("fold_r{i}"), &acc_ty);
                let mut fe = env(
                    &f,
                    callee,
                    types_of(&f, &fa),
                    vec![(acc_ty.clone(), r.clone())],
                );
                let post = assume_conjuncts(&f.contract.post, &mut fe, &mut q);
                q.assertions.push(post);
                let k66 = format!("(bvadd {} (_ bv1 66))", zext(&k, 64, 66));
                let neg = invariant(
                    node,
                    v,
                    step_count(k66, 66),
                    symval_to_fval(&r, &acc_ty),
                    &mut e,
                    &mut q,
                    true,
                );
                q.assertions.push(neg);
                obs.push(Ob {
                    id: format!("{at}/FOLD.step"),
                    qb: q,
                    nonvacuous: false,
                });
                // The result: a fresh value with I(len(xs), result) (docs/05).
                let res = qb.fresh_symval(&format!("p{i}_fold"), &acc_ty);
                let len65 = zext(&coll_len_bv64(&args[1]), 64, 65);
                let mut e = fold_env(&qb);
                let mut q2 = std::mem::take(&mut qb);
                let fact = invariant(
                    node,
                    v,
                    step_count(len65, 65),
                    symval_to_fval(&res, &acc_ty),
                    &mut e,
                    &mut q2,
                    false,
                );
                q2.assertions.push(fact);
                qb = q2;
                fold_result = Some(vec![res]);
            }
            _ => {}
        }

        // Then the op's results and facts join the context.
        let results = match fold_result {
            Some(r) => r,
            None => eval_op_core(
                node,
                op,
                &args,
                &format!("p{i}"),
                DomainMode::Strict,
                callee,
                &mut qb,
                0,
            ),
        };
        qb.domains.clear();
        for (s, sv) in results.iter().enumerate() {
            ports.insert((i as u32, s as u8), (op.results[s].clone(), sv.clone()));
        }
        vals.push(results);
    }

    // POST, against the whole body.
    let results: Vec<(Type, SymVal)> = node
        .outputs
        .iter()
        .zip(&node.results)
        .map(|(o, t)| {
            (
                t.clone(),
                operand(node, *o, &inputs, &vals, &mut consts, &mut qb),
            )
        })
        .collect();
    for (j, t) in node.contract.post.iter().enumerate() {
        let mut q = qb.clone();
        let mut e = env(node, callee, inputs.clone(), results.clone());
        e.ports = ports.clone();
        let neg = encode_neg_goal(t, &mut e, &mut q);
        q.assertions.push(neg);
        obs.push(Ob {
            id: format!("@{label}/POST.{j}"),
            qb: q,
            nonvacuous: false,
        });
    }

    // Well-definedness (ADR 0010): PRE conjunct j assuming conjuncts 0..j; POST after the body.
    let mut pre_ctx = pre_base;
    for (j, t) in node.contract.pre.iter().enumerate() {
        for (w, wd) in collect_wd_obligations(t, node, callee).iter().enumerate() {
            let mut q = pre_ctx.clone();
            let mut e = env(node, callee, inputs.clone(), vec![]);
            let neg = encode_wd_obligation(wd, &mut e, &mut q);
            q.assertions.push(neg);
            obs.push(Ob {
                id: format!("@{label}/PRE.{j}.wd.{w}"),
                qb: q,
                nonvacuous: false,
            });
        }
        let mut e = env(node, callee, inputs.clone(), vec![]);
        let a = encode_assume(t, &mut e, &mut pre_ctx);
        pre_ctx.assertions.push(a);
    }
    for (j, t) in node.contract.post.iter().enumerate() {
        for (w, wd) in collect_wd_obligations(t, node, callee).iter().enumerate() {
            let mut q = qb.clone();
            let mut e = env(node, callee, inputs.clone(), results.clone());
            e.ports = ports.clone();
            let neg = encode_wd_obligation(wd, &mut e, &mut q);
            q.assertions.push(neg);
            obs.push(Ob {
                id: format!("@{label}/POST.{j}.wd.{w}"),
                qb: q,
                nonvacuous: false,
            });
        }
    }

    let mut z3 = Z3Session::new(&opts.z3, opts.timeout_ms);
    let input_syms: Vec<SymVal> = inputs.iter().map(|(_, s)| s.clone()).collect();
    let mut out: Vec<Outcome> = obs
        .into_iter()
        .map(|ob| solve(ob, node, callee, &input_syms, &mut z3))
        .collect();
    // COST is not encoded yet: each conjunct stays unproven rather than silently skipped.
    for j in 0..node.contract.cost.len() {
        out.push(Outcome {
            id: format!("@{label}/COST.{j}"),
            verdict: Verdict::Unknown,
            code: Some("VG-V0005"),
            solver: "none".into(),
            inputs: None,
            message: "COST obligations are not encoded yet".into(),
        });
    }
    out
}

fn solve(
    ob: Ob,
    node: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    input_syms: &[SymVal],
    z3: &mut Z3Session,
) -> Outcome {
    let mut o = Outcome {
        id: ob.id,
        verdict: Verdict::Unknown,
        code: Some("VG-V0005"),
        solver: z3.name().to_string(),
        inputs: None,
        message: String::new(),
    };
    let mut q = ob.qb;
    if let Some(e) = &q.unresolved_error {
        o.message = format!("not encoded: {e}");
        return o;
    }
    q.finalize_instantiations(callee);
    if let Some(e) = &q.unresolved_error {
        o.message = format!("not encoded: {e}");
        return o;
    }
    match z3.check_query(&q.into_pruned_query()) {
        CheckResult::Unsat if ob.nonvacuous => {
            o.verdict = Verdict::Vacuous;
            o.code = Some("VG-V0003");
            o.message = "no input satisfies PRE and the LEARNABLE box".into();
        }
        CheckResult::Unsat => {
            o.verdict = Verdict::Proven;
            o.code = None;
        }
        CheckResult::Sat if ob.nonvacuous => {
            o.verdict = Verdict::Proven;
            o.code = None;
            o.message = "satisfiable".into();
        }
        CheckResult::Sat => match decode_inputs(z3, node, input_syms) {
            DecodedModel::Values(v) => {
                o.verdict = match classify_sat_model(node, &o.id, &v, callee) {
                    ReplayVerdict::ConcreteCounterexample => Verdict::Counterexample,
                    ReplayVerdict::ContractGap => Verdict::ContractGap,
                    ReplayVerdict::PartialAssumptionViolation => Verdict::Unknown,
                    ReplayVerdict::VerifierBug => Verdict::Error,
                };
                o.code = Some(match o.verdict {
                    Verdict::Counterexample => "VG-V0001",
                    Verdict::ContractGap => "VG-V0002",
                    Verdict::Unknown => "VG-V0006",
                    _ => "VG-V0008",
                });
                o.message = "refuted by the solver's model".into();
                o.inputs = Some(v);
            }
            DecodedModel::TooLarge(m) => {
                o.code = Some("VG-V0007");
                o.message = m;
            }
            DecodedModel::Error(m) => {
                o.verdict = Verdict::Error;
                o.code = Some("VG-V0008");
                o.message = m;
            }
        },
        CheckResult::Unknown { timeout, reason } => {
            o.code = Some(if timeout { "VG-V0004" } else { "VG-V0005" });
            o.message = reason;
        }
        CheckResult::Error(m) => {
            o.verdict = Verdict::Error;
            o.code = Some("VG-V0008");
            o.message = m;
        }
    }
    o
}
