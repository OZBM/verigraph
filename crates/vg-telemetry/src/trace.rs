//! Obligation ID parsing, single-op tracing via `vg-interp`, concrete obligation checking,
//! and sparse docs/15 JSON value formatting.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value as J, json};
use vg_core::float;
use vg_core::ir::{Contract, Hash, Node, Op, Operand, Term};
use vg_core::opcode;
use vg_core::types::{EffectMask, Type};
use vg_core::value::Value;
use vg_interp::{Clause, NoEffectsHost, Trap};

/// Parsed form of a docs/15 obligation ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObligationKind {
    Post(usize),
    Cost(usize),
    NonVacuous,
    PreWd(usize, usize),
    PostWd(usize, usize),
    OpDomain {
        op_idx: u32,
        op_name: String,
        kind: String,
    },
    CallPre {
        op_idx: u32,
        pre_idx: usize,
    },
    MapPre {
        op_idx: u32,
        pre_idx: usize,
    },
    GenPre {
        op_idx: u32,
        pre_idx: usize,
    },
    FoldBase {
        op_idx: u32,
    },
    FoldStep {
        op_idx: u32,
    },
    FoldPre {
        op_idx: u32,
        pre_idx: usize,
    },
    GuardThenPre {
        op_idx: u32,
        pre_idx: usize,
    },
    GuardElsePre {
        op_idx: u32,
        pre_idx: usize,
    },
    AssertGuard {
        op_idx: u32,
    },
}

/// Parses a docs/15 obligation ID such as `@soft_knee/POST.0` or `@f/%0/INDEX.in_bounds`.
pub fn parse_obligation_id(id: &str) -> Option<ObligationKind> {
    let rest = id.split_once('/')?.1;
    if rest == "NONVACUOUS" {
        return Some(ObligationKind::NonVacuous);
    }
    if let Some(s) = rest.strip_prefix("POST.") {
        if let Some((j, w)) = s.split_once(".wd.") {
            return Some(ObligationKind::PostWd(j.parse().ok()?, w.parse().ok()?));
        }
        return Some(ObligationKind::Post(s.parse().ok()?));
    }
    if let Some(s) = rest.strip_prefix("PRE.")
        && let Some((j, w)) = s.split_once(".wd.")
    {
        return Some(ObligationKind::PreWd(j.parse().ok()?, w.parse().ok()?));
    }
    if let Some(s) = rest.strip_prefix("COST.") {
        return Some(ObligationKind::Cost(s.parse().ok()?));
    }
    if let Some(port_rest) = rest.strip_prefix('%') {
        let (op_str, tail) = port_rest.split_once('/')?;
        let op_idx: u32 = op_str.parse().ok()?;
        if tail == "ASSERT_GUARD" {
            return Some(ObligationKind::AssertGuard { op_idx });
        }
        if tail == "FOLD.base" {
            return Some(ObligationKind::FoldBase { op_idx });
        }
        if tail == "FOLD.step" {
            return Some(ObligationKind::FoldStep { op_idx });
        }
        if let Some(j) = tail.strip_prefix("FOLD.pre.") {
            return Some(ObligationKind::FoldPre {
                op_idx,
                pre_idx: j.parse().ok()?,
            });
        }
        if let Some(j) = tail.strip_prefix("CALL.pre.") {
            return Some(ObligationKind::CallPre {
                op_idx,
                pre_idx: j.parse().ok()?,
            });
        }
        if let Some(j) = tail.strip_prefix("MAP.pre.") {
            return Some(ObligationKind::MapPre {
                op_idx,
                pre_idx: j.parse().ok()?,
            });
        }
        if let Some(j) = tail.strip_prefix("GEN.pre.") {
            return Some(ObligationKind::GenPre {
                op_idx,
                pre_idx: j.parse().ok()?,
            });
        }
        if let Some(j) = tail.strip_prefix("GUARD.then.pre.") {
            return Some(ObligationKind::GuardThenPre {
                op_idx,
                pre_idx: j.parse().ok()?,
            });
        }
        if let Some(j) = tail.strip_prefix("GUARD.else.pre.") {
            return Some(ObligationKind::GuardElsePre {
                op_idx,
                pre_idx: j.parse().ok()?,
            });
        }
        if let Some((op_name, kind)) = tail.split_once('.') {
            return Some(ObligationKind::OpDomain {
                op_idx,
                op_name: op_name.to_string(),
                kind: kind.to_string(),
            });
        }
    }
    None
}

impl ObligationKind {
    pub fn op_index(&self) -> Option<u32> {
        match self {
            ObligationKind::OpDomain { op_idx, .. }
            | ObligationKind::CallPre { op_idx, .. }
            | ObligationKind::MapPre { op_idx, .. }
            | ObligationKind::GenPre { op_idx, .. }
            | ObligationKind::FoldBase { op_idx }
            | ObligationKind::FoldStep { op_idx }
            | ObligationKind::FoldPre { op_idx, .. }
            | ObligationKind::GuardThenPre { op_idx, .. }
            | ObligationKind::GuardElsePre { op_idx, .. }
            | ObligationKind::AssertGuard { op_idx } => Some(*op_idx),
            _ => None,
        }
    }
}

/// Result of evaluating a node op-by-op via `vg-interp`.
#[derive(Clone, Debug)]
pub struct Trace {
    pub op_values: Vec<Option<Vec<Value>>>,
    pub outputs: Option<Vec<Value>>,
    pub trap: Option<(u32, Trap)>,
    pub accessed_indices: BTreeMap<Operand, BTreeSet<usize>>,
}

/// Result of checking whether a specific obligation is concretely violated on `inputs`.
#[derive(Clone, Debug, Default)]
pub struct ViolationCheck {
    pub pre_holds: bool,
    pub violated: bool,
    pub step: Option<u64>,
    pub extra_indices: BTreeMap<Operand, BTreeSet<usize>>,
}

/// Decodes all non-Int constants of `node`.
pub fn decode_consts(node: &Node) -> Vec<Option<Value>> {
    node.consts
        .iter()
        .map(|c| {
            if c.ty == Type::Int {
                None
            } else {
                Value::from_bytes(&c.bytes, &c.ty).map(|(v, _)| v)
            }
        })
        .collect()
}

pub fn read_operand(
    o: Operand,
    inputs: &[Value],
    consts: &[Option<Value>],
    op_values: &[Option<Vec<Value>>],
) -> Option<Value> {
    match o {
        Operand::Input(i) => inputs.get(i as usize).cloned(),
        Operand::Op(i, s) => op_values
            .get(i as usize)
            .and_then(|v| v.as_ref())
            .and_then(|rs| rs.get(s as usize))
            .cloned(),
        Operand::Const(c) => consts.get(c as usize).and_then(|v| v.clone()),
        Operand::Callee(_) | Operand::Invariant(_) => None,
    }
}

/// Evaluates `node` one op at a time using `vg_interp::run`, stopping at the first trap.
pub fn trace_eval(node: &Node, inputs: &[Value], callee: &dyn Fn(&Hash) -> Option<Node>) -> Trace {
    trace_eval_with_overrides(node, inputs, callee, &BTreeMap::new())
}

/// Evaluates `node` one op at a time using `vg_interp::run`, substituting `overrides` for the
/// specified canonical op indices instead of executing them.
pub fn trace_eval_with_overrides(
    node: &Node,
    inputs: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
    overrides: &BTreeMap<u32, Vec<Value>>,
) -> Trace {
    let consts = decode_consts(node);
    let mut op_values: Vec<Option<Vec<Value>>> = vec![None; node.ops.len()];
    let mut trap_info: Option<(u32, Trap)> = None;
    let mut accessed_indices: BTreeMap<Operand, BTreeSet<usize>> = BTreeMap::new();

    for (idx, op) in node.ops.iter().enumerate() {
        let i_u32 = idx as u32;
        if let Some(ov) = overrides.get(&i_u32) {
            op_values[idx] = Some(ov.clone());
            continue;
        }

        // Record concrete collection indices accessed by INDEX / UPDATE / SLICE.
        let info = opcode::by_code(op.opcode);
        match info.map(|x| x.name) {
            Some("INDEX" | "UPDATE") => {
                if let Some(Value::Int(k)) =
                    read_operand(op.operands[1], inputs, &consts, &op_values)
                    && let Ok(ku) = usize::try_from(k)
                {
                    accessed_indices
                        .entry(op.operands[0])
                        .or_default()
                        .insert(ku);
                }
            }
            Some("SLICE") => {
                if let (Some(Value::Int(lo)), Some(Value::Int(hi))) = (
                    read_operand(op.operands[1], inputs, &consts, &op_values),
                    read_operand(op.operands[2], inputs, &consts, &op_values),
                ) {
                    if let Ok(lu) = usize::try_from(lo) {
                        accessed_indices
                            .entry(op.operands[0])
                            .or_default()
                            .insert(lu);
                    }
                    if hi > lo
                        && let Ok(hu) = usize::try_from(hi - 1)
                    {
                        accessed_indices
                            .entry(op.operands[0])
                            .or_default()
                            .insert(hu);
                    }
                }
            }
            _ => {}
        }

        match eval_single_op(node, op, inputs, &consts, &op_values, callee) {
            Ok(res) => op_values[idx] = Some(res),
            Err(t) => {
                trap_info = Some((i_u32, t));
                break;
            }
        }
    }

    let outputs = if trap_info.is_none() {
        node.outputs
            .iter()
            .map(|o| read_operand(*o, inputs, &consts, &op_values))
            .collect::<Option<Vec<Value>>>()
    } else {
        None
    };

    Trace {
        op_values,
        outputs,
        trap: trap_info,
        accessed_indices,
    }
}

fn eval_single_op(
    node: &Node,
    op: &Op,
    inputs: &[Value],
    consts: &[Option<Value>],
    op_values: &[Option<Vec<Value>>],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Result<Vec<Value>, Trap> {
    let mut single_params = Vec::new();
    let mut single_args = Vec::new();
    let mut new_operands = Vec::with_capacity(op.operands.len());

    for &o in &op.operands {
        match o {
            Operand::Callee(_) | Operand::Invariant(_) => new_operands.push(o),
            _ => {
                let ty = node
                    .operand_type(o)
                    .cloned()
                    .ok_or_else(|| Trap::entry("missing operand type during trace"))?;
                let val = read_operand(o, inputs, consts, op_values)
                    .ok_or_else(|| Trap::entry("missing operand value during trace"))?;
                let slot = single_params.len() as u32;
                single_params.push(ty);
                single_args.push(val);
                new_operands.push(Operand::Input(slot));
            }
        }
    }

    let single_node = Node {
        spec: false,
        effects: EffectMask(node.effects.0),
        params: single_params,
        results: op.results.clone(),
        learnable: Vec::new(),
        consts: Vec::new(),
        calls: node.calls.clone(),
        ops: vec![Op {
            opcode: op.opcode,
            mode: op.mode,
            operands: new_operands,
            results: op.results.clone(),
        }],
        outputs: (0..op.results.len() as u8)
            .map(|s| Operand::Op(0, s))
            .collect(),
        contract: Contract::default(),
        debug: None,
    };

    let mut host = NoEffectsHost;
    vg_interp::run(&single_node, &single_args, callee, &mut host)
}

/// Returns true when `inputs` satisfy `PRE` and every `LEARNABLE` parameter lies in its declared box.
pub fn pre_and_learnable_hold(
    node: &Node,
    inputs: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> bool {
    if inputs.len() != node.input_count() {
        return false;
    }
    for (idx, l) in node.learnable.iter().enumerate() {
        let Some(Value::Float(bits)) = inputs.get(node.params.len() + idx) else {
            return false;
        };
        let lo = read_float_const(
            &node.consts[l.lo as usize].ty,
            &node.consts[l.lo as usize].bytes,
        );
        let hi = read_float_const(
            &node.consts[l.hi as usize].ty,
            &node.consts[l.hi as usize].bytes,
        );
        let v = float::to_f64(&l.ty, *bits);
        if v.is_nan() || !(lo <= v && v <= hi) {
            return false;
        }
    }
    vg_interp::holds(node, Clause::Pre, inputs, &[], callee) == Ok(true)
}

fn read_float_const(ty: &Type, bytes: &[u8]) -> f64 {
    let mut w = [0u8; 8];
    w[..bytes.len()].copy_from_slice(bytes);
    float::to_f64(ty, u64::from_le_bytes(w))
}

fn eval_single_pre_conjunct(
    node: &Node,
    pre_idx: usize,
    inputs: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Result<bool, Trap> {
    let Some(term) = node.contract.pre.get(pre_idx) else {
        return Ok(true);
    };
    let mut shell = node.clone();
    shell.contract = Contract {
        pre: vec![term.clone()],
        ..Contract::default()
    };
    vg_interp::holds(&shell, Clause::Pre, inputs, &[], callee)
}

fn eval_single_post_conjunct(
    node: &Node,
    post_idx: usize,
    inputs: &[Value],
    results: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Result<bool, Trap> {
    let Some(term) = node.contract.post.get(post_idx) else {
        return Ok(true);
    };
    let mut shell = node.clone();
    shell.contract = Contract {
        post: vec![term.clone()],
        ..Contract::default()
    };
    vg_interp::holds(&shell, Clause::Post, inputs, results, callee)
}

/// Checks concretely whether `obligation` fails on `inputs` given its `trace`.
pub fn check_violation(
    node: &Node,
    kind: &ObligationKind,
    inputs: &[Value],
    trace: &Trace,
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> ViolationCheck {
    let mut out = ViolationCheck {
        pre_holds: pre_and_learnable_hold(node, inputs, callee),
        violated: false,
        step: None,
        extra_indices: BTreeMap::new(),
    };
    if !out.pre_holds {
        return out;
    }

    let consts = decode_consts(node);

    match kind {
        ObligationKind::Post(j) => {
            if trace.trap.is_some() {
                out.violated = true;
            } else if let Some(results) = &trace.outputs {
                out.violated =
                    eval_single_post_conjunct(node, *j, inputs, results, callee) == Ok(false);
            }
            if let Some(term) = node.contract.post.get(*j) {
                collect_const_formula_indices(node, term, &mut out.extra_indices);
            }
        }
        ObligationKind::OpDomain { op_idx, .. } => {
            if let Some((t_idx, _)) = &trace.trap {
                out.violated = *t_idx <= *op_idx;
            }
        }
        ObligationKind::CallPre { op_idx, pre_idx } => {
            if let Some((t_idx, _)) = &trace.trap
                && *t_idx < *op_idx
            {
                out.violated = true;
                return out;
            }
            if let Some(op) = node.ops.get(*op_idx as usize)
                && let Some(Operand::Callee(c)) = op.operands.first()
                && let Some(h) = node.calls.get(*c as usize)
                && let Some(target) = callee(h)
            {
                let mut args = Vec::new();
                for o in &op.operands[1..] {
                    if let Some(v) = read_operand(*o, inputs, &consts, &trace.op_values) {
                        args.push(v);
                    }
                }
                if args.len() == target.params.len() && target.learnable.is_empty() {
                    out.violated =
                        eval_single_pre_conjunct(&target, *pre_idx, &args, callee) == Ok(false);
                } else if trace.trap.as_ref().is_some_and(|(i, _)| *i == *op_idx) {
                    out.violated = true;
                }
            }
        }
        ObligationKind::MapPre { op_idx, pre_idx } => {
            if let Some((t_idx, _)) = &trace.trap
                && *t_idx < *op_idx
            {
                out.violated = true;
                return out;
            }
            if let Some(op) = node.ops.get(*op_idx as usize)
                && let Some(Operand::Callee(c)) = op.operands.first()
                && let Some(h) = node.calls.get(*c as usize)
                && let Some(kernel) = callee(h)
                && let Some(Value::Seq(xs)) =
                    read_operand(op.operands[1], inputs, &consts, &trace.op_values)
            {
                let mut bcasts = Vec::new();
                for o in &op.operands[2..] {
                    if let Some(v) = read_operand(*o, inputs, &consts, &trace.op_values) {
                        bcasts.push(v);
                    }
                }
                for (elem_idx, elem) in xs.into_iter().enumerate() {
                    let mut args = Vec::with_capacity(1 + bcasts.len());
                    args.push(elem);
                    args.extend(bcasts.iter().cloned());
                    if eval_single_pre_conjunct(&kernel, *pre_idx, &args, callee) == Ok(false) {
                        out.violated = true;
                        out.extra_indices
                            .entry(op.operands[1])
                            .or_default()
                            .insert(elem_idx);
                        break;
                    }
                }
            }
        }
        ObligationKind::GenPre { op_idx, pre_idx } => {
            if let Some((t_idx, _)) = &trace.trap
                && *t_idx < *op_idx
            {
                out.violated = true;
                return out;
            }
            if let Some(op) = node.ops.get(*op_idx as usize)
                && let Some(Operand::Callee(c)) = op.operands.first()
                && let Some(h) = node.calls.get(*c as usize)
                && let Some(kernel) = callee(h)
                && let Some(Value::Int(n)) =
                    read_operand(op.operands[1], inputs, &consts, &trace.op_values)
                && n >= 0
            {
                let mut bcasts = Vec::new();
                for o in &op.operands[2..] {
                    if let Some(v) = read_operand(*o, inputs, &consts, &trace.op_values) {
                        bcasts.push(v);
                    }
                }
                let cap = n.min(4096) as usize;
                for i in 0..cap {
                    let mut args = Vec::with_capacity(1 + bcasts.len());
                    args.push(Value::Int(i as i128));
                    args.extend(bcasts.iter().cloned());
                    if eval_single_pre_conjunct(&kernel, *pre_idx, &args, callee) == Ok(false) {
                        out.violated = true;
                        out.step = Some(i as u64);
                        break;
                    }
                }
            }
        }
        ObligationKind::GuardThenPre { op_idx, pre_idx } => {
            if let Some((t_idx, _)) = &trace.trap
                && *t_idx < *op_idx
            {
                out.violated = true;
                return out;
            }
            if let Some(op) = node.ops.get(*op_idx as usize)
                && let Some(Value::Bool(true)) =
                    read_operand(op.operands[0], inputs, &consts, &trace.op_values)
                && let Some(Operand::Callee(c)) = op.operands.get(1)
                && let Some(h) = node.calls.get(*c as usize)
                && let Some(branch) = callee(h)
            {
                let last = op.operands.len() - 1;
                let mut args = Vec::new();
                for o in &op.operands[2..last] {
                    if let Some(v) = read_operand(*o, inputs, &consts, &trace.op_values) {
                        args.push(v);
                    }
                }
                if args.len() == branch.params.len() && branch.learnable.is_empty() {
                    out.violated =
                        eval_single_pre_conjunct(&branch, *pre_idx, &args, callee) == Ok(false);
                }
            }
        }
        ObligationKind::GuardElsePre { op_idx, pre_idx } => {
            if let Some((t_idx, _)) = &trace.trap
                && *t_idx < *op_idx
            {
                out.violated = true;
                return out;
            }
            if let Some(op) = node.ops.get(*op_idx as usize)
                && let Some(Value::Bool(false)) =
                    read_operand(op.operands[0], inputs, &consts, &trace.op_values)
                && let Some(Operand::Callee(c)) = op.operands.last()
                && let Some(h) = node.calls.get(*c as usize)
                && let Some(branch) = callee(h)
            {
                let last = op.operands.len() - 1;
                let mut args = Vec::new();
                for o in &op.operands[2..last] {
                    if let Some(v) = read_operand(*o, inputs, &consts, &trace.op_values) {
                        args.push(v);
                    }
                }
                if args.len() == branch.params.len() && branch.learnable.is_empty() {
                    out.violated =
                        eval_single_pre_conjunct(&branch, *pre_idx, &args, callee) == Ok(false);
                }
            }
        }
        ObligationKind::AssertGuard { op_idx } => {
            if let Some((t_idx, _)) = &trace.trap
                && *t_idx < *op_idx
            {
                out.violated = true;
                return out;
            }
            if let Some(op) = node.ops.get(*op_idx as usize)
                && let Some(Value::Bool(false)) =
                    read_operand(op.operands[0], inputs, &consts, &trace.op_values)
            {
                out.violated = true;
            }
        }
        ObligationKind::FoldBase { op_idx }
        | ObligationKind::FoldStep { op_idx }
        | ObligationKind::FoldPre { op_idx, .. } => {
            if let Some((t_idx, _)) = &trace.trap
                && *t_idx < *op_idx
            {
                out.violated = true;
                return out;
            }
            if let Some(op) = node.ops.get(*op_idx as usize)
                && op.operands.len() >= 4
                && let (Some(Operand::Callee(c)), Some(Operand::Invariant(inv_idx))) =
                    (op.operands.first(), op.operands.last())
                && let Some(h) = node.calls.get(*c as usize)
                && let Some(kernel) = callee(h)
                && let Some(Value::Seq(xs)) =
                    read_operand(op.operands[1], inputs, &consts, &trace.op_values)
                && let Some(init) = read_operand(
                    op.operands[op.operands.len() - 2],
                    inputs,
                    &consts,
                    &trace.op_values,
                )
            {
                let mut bcasts = Vec::new();
                for o in &op.operands[2..op.operands.len() - 2] {
                    if let Some(v) = read_operand(*o, inputs, &consts, &trace.op_values) {
                        bcasts.push(v);
                    }
                }
                let port_vals: Vec<Vec<Value>> = trace
                    .op_values
                    .iter()
                    .map(|ov| ov.clone().unwrap_or_default())
                    .collect();

                if matches!(kind, ObligationKind::FoldBase { .. }) {
                    if vg_interp::holds_invariant(
                        node, *inv_idx, 0, &init, inputs, &port_vals, callee,
                    ) == Ok(false)
                    {
                        out.violated = true;
                        out.step = Some(0);
                    }
                    return out;
                }

                let mut acc = init;
                let mut host = NoEffectsHost;
                for (step, elem) in xs.into_iter().enumerate() {
                    let inv_before = vg_interp::holds_invariant(
                        node,
                        *inv_idx,
                        step as i128,
                        &acc,
                        inputs,
                        &port_vals,
                        callee,
                    ) == Ok(true);
                    let mut args = Vec::with_capacity(2 + bcasts.len());
                    args.push(acc.clone());
                    args.push(elem);
                    args.extend(bcasts.iter().cloned());

                    if let ObligationKind::FoldPre { pre_idx, .. } = kind
                        && inv_before
                        && eval_single_pre_conjunct(&kernel, *pre_idx, &args, callee) == Ok(false)
                    {
                        out.violated = true;
                        out.step = Some(step as u64);
                        out.extra_indices
                            .entry(op.operands[1])
                            .or_default()
                            .insert(step);
                        return out;
                    }

                    let Ok(res) = vg_interp::run(&kernel, &args, callee, &mut host) else {
                        out.violated = true;
                        out.step = Some(step as u64);
                        out.extra_indices
                            .entry(op.operands[1])
                            .or_default()
                            .insert(step);
                        return out;
                    };
                    let Some(next_acc) = res.into_iter().next() else {
                        return out;
                    };

                    if matches!(kind, ObligationKind::FoldStep { .. })
                        && inv_before
                        && vg_interp::holds_invariant(
                            node,
                            *inv_idx,
                            (step + 1) as i128,
                            &next_acc,
                            inputs,
                            &port_vals,
                            callee,
                        ) == Ok(false)
                    {
                        out.violated = true;
                        out.step = Some(step as u64);
                        out.extra_indices
                            .entry(op.operands[1])
                            .or_default()
                            .insert(step);
                        return out;
                    }
                    acc = next_acc;
                }
            }
        }
        ObligationKind::PreWd(j, _) => {
            if let Some(term) = node.contract.pre.get(*j) {
                let mut shell = node.clone();
                shell.contract = Contract {
                    pre: vec![term.clone()],
                    ..Contract::default()
                };
                if vg_interp::holds(&shell, Clause::Pre, inputs, &[], callee).is_err() {
                    out.violated = true;
                }
            }
        }
        ObligationKind::PostWd(j, _) => {
            if trace.trap.is_some() {
                out.violated = true;
            } else if let Some(results) = &trace.outputs
                && let Some(term) = node.contract.post.get(*j)
            {
                let mut shell = node.clone();
                shell.contract = Contract {
                    post: vec![term.clone()],
                    ..Contract::default()
                };
                if vg_interp::holds(&shell, Clause::Post, inputs, results, callee).is_err() {
                    out.violated = true;
                }
            }
        }
        ObligationKind::Cost(_) | ObligationKind::NonVacuous => {}
    }
    out
}

fn collect_const_formula_indices(
    node: &Node,
    term: &Term,
    out: &mut BTreeMap<Operand, BTreeSet<usize>>,
) {
    match term {
        Term::Index(base, idxs) => {
            if let Term::Input(inp) = base.as_ref()
                && idxs.len() == 1
                && let Term::Const(c) = &idxs[0]
                && let Some(kc) = node.consts.get(*c as usize)
                && let Some(v) = vg_core::lower::int_value(&kc.bytes)
                && let Ok(u) = usize::try_from(v)
            {
                out.entry(Operand::Input(*inp)).or_default().insert(u);
            }
            collect_const_formula_indices(node, base, out);
            for i in idxs {
                collect_const_formula_indices(node, i, out);
            }
        }
        Term::Not(a) | Term::Neg(a) | Term::Proj(a, _) => {
            collect_const_formula_indices(node, a, out);
        }
        Term::Bin(_, a, b) => {
            collect_const_formula_indices(node, a, out);
            collect_const_formula_indices(node, b, out);
        }
        Term::Forall(rs, b) => {
            for (l, h) in rs {
                collect_const_formula_indices(node, l, out);
                collect_const_formula_indices(node, h, out);
            }
            collect_const_formula_indices(node, b, out);
        }
        Term::Builtin(_, args)
        | Term::Spec(_, args)
        | Term::Fold(_, args)
        | Term::Apply(_, args) => {
            for a in args {
                collect_const_formula_indices(node, a, out);
            }
        }
        _ => {}
    }
}

/// Encodes `val` at type `ty` into docs/15 JSON, applying the sparse rule for collections
/// longer than 16 elements: `{"at": {"0": ..., ...}, "len": N}`.
pub fn value_to_report_json(val: &Value, ty: &Type, read_indices: &BTreeSet<usize>) -> J {
    match (val, ty) {
        (Value::Seq(xs), Type::Array(elem, _) | Type::Slice(elem)) if xs.len() > 16 => {
            sparse_seq_json(xs, elem, read_indices)
        }
        (Value::Seq(xs), Type::Tensor(elem, _)) if xs.len() > 16 => {
            sparse_seq_json(xs, elem, read_indices)
        }
        (Value::Tuple(vs), Type::Tuple(ts)) if vs.len() == ts.len() => J::Array(
            vs.iter()
                .zip(ts)
                .map(|(v, t)| value_to_report_json(v, t, &BTreeSet::new()))
                .collect(),
        ),
        (Value::Ok(v), Type::Result(ok_t, _)) => {
            json!({ "ok": value_to_report_json(v, ok_t, read_indices) })
        }
        (Value::Err(e), Type::Result(_, err_t)) => {
            json!({ "err": value_to_report_json(e, err_t, read_indices) })
        }
        _ => val
            .to_json(ty)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(J::Null),
    }
}

fn sparse_seq_json(xs: &[Value], elem_ty: &Type, read_indices: &BTreeSet<usize>) -> J {
    let mut keep = BTreeSet::new();
    keep.insert(0usize);
    keep.insert(xs.len() - 1);
    for &idx in read_indices {
        if idx < xs.len() {
            keep.insert(idx);
        }
    }
    let mut at = Map::new();
    for idx in keep {
        at.insert(
            idx.to_string(),
            value_to_report_json(&xs[idx], elem_ty, &BTreeSet::new()),
        );
    }
    let mut obj = Map::new();
    obj.insert("at".to_string(), J::Object(at));
    obj.insert("len".to_string(), J::from(xs.len() as u64));
    J::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_obligation_id_forms() {
        assert_eq!(
            parse_obligation_id("@soft_knee/POST.0"),
            Some(ObligationKind::Post(0))
        );
        assert_eq!(
            parse_obligation_id("@limit_frame/COST.1"),
            Some(ObligationKind::Cost(1))
        );
        assert_eq!(
            parse_obligation_id("@n/NONVACUOUS"),
            Some(ObligationKind::NonVacuous)
        );
        assert_eq!(
            parse_obligation_id("@n/POST.0.wd.2"),
            Some(ObligationKind::PostWd(0, 2))
        );
        assert_eq!(
            parse_obligation_id("@n/PRE.1.wd.0"),
            Some(ObligationKind::PreWd(1, 0))
        );
        assert_eq!(
            parse_obligation_id("@safe_div/%2/DIV.nonzero"),
            Some(ObligationKind::OpDomain {
                op_idx: 2,
                op_name: "DIV".to_string(),
                kind: "nonzero".to_string(),
            })
        );
        assert_eq!(
            parse_obligation_id("@f/%0/CALL.pre.1"),
            Some(ObligationKind::CallPre {
                op_idx: 0,
                pre_idx: 1,
            })
        );
        assert_eq!(
            parse_obligation_id("@f/%3/MAP.pre.0"),
            Some(ObligationKind::MapPre {
                op_idx: 3,
                pre_idx: 0,
            })
        );
        assert_eq!(
            parse_obligation_id("@f/%4/GEN.pre.2"),
            Some(ObligationKind::GenPre {
                op_idx: 4,
                pre_idx: 2,
            })
        );
        assert_eq!(
            parse_obligation_id("@sum/%0/FOLD.base"),
            Some(ObligationKind::FoldBase { op_idx: 0 })
        );
        assert_eq!(
            parse_obligation_id("@sum/%0/FOLD.step"),
            Some(ObligationKind::FoldStep { op_idx: 0 })
        );
        assert_eq!(
            parse_obligation_id("@sum/%0/FOLD.pre.0"),
            Some(ObligationKind::FoldPre {
                op_idx: 0,
                pre_idx: 0,
            })
        );
        assert_eq!(
            parse_obligation_id("@g/%1/GUARD.then.pre.0"),
            Some(ObligationKind::GuardThenPre {
                op_idx: 1,
                pre_idx: 0,
            })
        );
        assert_eq!(
            parse_obligation_id("@g/%1/GUARD.else.pre.0"),
            Some(ObligationKind::GuardElsePre {
                op_idx: 1,
                pre_idx: 0,
            })
        );
        assert_eq!(
            parse_obligation_id("@g/%5/ASSERT_GUARD"),
            Some(ObligationKind::AssertGuard { op_idx: 5 })
        );
    }

    #[test]
    fn sparse_collection_formats_over_16_elements() {
        let xs: Vec<Value> = (0..20).map(Value::Int).collect();
        let ty = Type::Array(Box::new(Type::UInt(32)), 20);
        let mut reads = BTreeSet::new();
        reads.insert(7);
        let j = value_to_report_json(&Value::Seq(xs.clone()), &ty, &reads);
        assert_eq!(j["len"], 20);
        let at = j["at"].as_object().unwrap();
        assert_eq!(at.len(), 3);
        assert_eq!(at["0"], 0);
        assert_eq!(at["7"], 7);
        assert_eq!(at["19"], 19);

        let short: Vec<Value> = (0..16).map(Value::Int).collect();
        let short_ty = Type::Array(Box::new(Type::UInt(32)), 16);
        let j_short = value_to_report_json(&Value::Seq(short), &short_ty, &reads);
        assert!(j_short.is_array());
        assert_eq!(j_short.as_array().unwrap().len(), 16);
    }
}
