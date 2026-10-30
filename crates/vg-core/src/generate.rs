//! Seeded generation for property, differential and fuzz tests (docs/16), behind the `gen`
//! feature: random equivalence moves on VG-Sym (docs/13) and random well-typed programs (P1.11).

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::lexer;

/// SplitMix64: small, seeded and the same on every machine.
#[derive(Clone, Debug)]
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform below n (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.next_u64() % den < num
    }
}

/// Ops that never touch the environment, whose copies canonicalization merges and drops; calls
/// are pure when their callee is (see `effectful_labels`).
fn pure_stmt(op: &str) -> bool {
    !matches!(op, "FILE_READ" | "FILE_WRITE" | "CLOCK_READ")
}

const COMMUTATIVE: &[&str] = &[
    "ADD", "MUL", "AND", "OR", "XOR", "EQ", "NE", "BAND", "BOR", "FADD", "FMUL", "FMIN", "FMAX",
    "FMA",
];

/// A random variant of a module by 3 to 8 equivalence moves of docs/13, and the moves applied.
pub fn variant(m: &Module, seed: u64) -> (Module, Vec<&'static str>) {
    let mut rng = Rng(seed);
    let mut m = m.clone();
    let mut log = Vec::new();
    let effectful = effectful_labels(&m);
    for _ in 0..3 + rng.below(6) {
        let mv = rng.below(9);
        let Some(ni) = pick_node(&m, &mut rng) else {
            break;
        };
        let done = match mv {
            0 => rename_ports(&mut m, ni, &mut rng),
            1 => rename_names(&mut m, ni, &mut rng),
            2 => reorder_stmts(&mut m, ni, &mut rng),
            3 => duplicate_op(&mut m, ni, &mut rng, &effectful, true),
            4 => duplicate_op(&mut m, ni, &mut rng, &effectful, false),
            5 => swap_commutative(&mut m, ni, &mut rng),
            6 => bind_identity(&mut m, ni, &mut rng),
            7 => split_or_move_clauses(&mut m, ni, &mut rng),
            _ => rename_label(&mut m, &mut rng) || change_intent(&mut m, ni, &mut rng),
        };
        if done {
            log.push(
                [
                    "rename ports",
                    "rename names",
                    "reorder statements",
                    "duplicate op",
                    "add dead op",
                    "swap commutative operands",
                    "bind through IDENTITY",
                    "split or reorder clauses",
                    "rename label or intent",
                ][mv],
            );
        }
    }
    (m, log)
}

fn pick_node(m: &Module, rng: &mut Rng) -> Option<usize> {
    let nodes: Vec<usize> = (0..m.items.len())
        .filter(|i| matches!(m.items[*i], Item::Node(_)))
        .collect();
    (!nodes.is_empty()).then(|| nodes[rng.below(nodes.len())])
}

fn node_mut(m: &mut Module, i: usize) -> &mut NodeDef {
    match &mut m.items[i] {
        Item::Node(n) => n,
        Item::Query(_) => unreachable!("picked a node"),
    }
}

/// Labels of nodes and queries whose effects go beyond ARENA: calls to them are never moved.
fn effectful_labels(m: &Module) -> HashSet<String> {
    m.items
        .iter()
        .filter_map(|it| match it {
            Item::Node(n) if n.effects.iter().any(|e| e.text != "ARENA") => Some(n.label.clone()),
            Item::Query(q) if q.effects.iter().any(|e| e.text != "ARENA") => Some(q.label.clone()),
            _ => None,
        })
        .collect()
}

fn fresh(rng: &mut Rng, prefix: &str) -> String {
    format!("{prefix}{:x}", rng.next_u64() & 0xFFFF_FFFF)
}

/// Every port use in a statement's arguments or RETURN.
fn port_uses(s: &mut Stmt) -> Vec<&mut Name> {
    match s {
        Stmt::Assign { args, .. } => args
            .iter_mut()
            .filter_map(|a| match &mut a.val {
                ArgVal::Value(Value::Port(p)) => Some(p),
                _ => None,
            })
            .collect(),
        Stmt::Return { values, .. } => values
            .iter_mut()
            .filter_map(|v| match v {
                Value::Port(p) => Some(p),
                _ => None,
            })
            .collect(),
    }
}

fn walk_formula(f: &mut Formula, g: &mut dyn FnMut(&mut FKind)) {
    g(&mut f.kind);
    match &mut f.kind {
        FKind::Call { args, .. } => args.iter_mut().for_each(|a| walk_formula(a, g)),
        FKind::Forall { ranges, body } => {
            for r in ranges {
                walk_formula(&mut r.lo, g);
                walk_formula(&mut r.hi, g);
            }
            walk_formula(body, g);
        }
        FKind::Unary(_, a) | FKind::Proj(a, _) => walk_formula(a, g),
        FKind::Binary(_, a, b) => {
            walk_formula(a, g);
            walk_formula(b, g);
        }
        FKind::Index(a, idx) => {
            walk_formula(a, g);
            idx.iter_mut().for_each(|i| walk_formula(i, g));
        }
        FKind::Value(_) | FKind::Num(_) | FKind::Result => {}
    }
}

fn rename_ports(m: &mut Module, ni: usize, rng: &mut Rng) -> bool {
    let n = node_mut(m, ni);
    let mut map: HashMap<String, String> = HashMap::new();
    let base = 1000 + rng.below(9000);
    for s in &n.body {
        if let Stmt::Assign { binds, .. } = s {
            for b in binds {
                let k = map.len();
                map.insert(b.port.text.clone(), (base + 7 * k).to_string());
            }
        }
    }
    for s in &mut n.body {
        if let Stmt::Assign { binds, .. } = s {
            binds
                .iter_mut()
                .for_each(|b| b.port.text = map[&b.port.text].clone());
        }
        for p in port_uses(s) {
            if let Some(new) = map.get(&p.text) {
                p.text = new.clone();
            }
        }
    }
    for c in &mut n.clauses {
        walk_formula(&mut c.formula, &mut |k| {
            if let FKind::Value(Value::Port(p)) = k
                && let Some(new) = map.get(&p.text)
            {
                p.text = new.clone();
            }
        });
    }
    !map.is_empty()
}

const RESOURCES: &[&str] = &[
    "heap_bytes",
    "arena_bytes",
    "stack_bytes",
    "ops",
    "latency_p99_us",
];

/// Renames parameters, θ, forall variables, INVARIANT bound names and labels, and refinement
/// variables, each to a fresh name.
fn rename_names(m: &mut Module, ni: usize, rng: &mut Rng) -> bool {
    let n = node_mut(m, ni);
    let mut params: HashMap<String, String> = HashMap::new();
    for p in &mut n.params {
        let new = fresh(rng, "p");
        params.insert(p.name.text.clone(), new.clone());
        p.name.text = new;
    }
    for l in n.learnable.iter_mut().flatten() {
        let new = fresh(rng, "q");
        params.insert(l.name.text.clone(), new.clone());
        l.name.text = new;
    }
    for s in &mut n.body {
        let vals: Vec<&mut Value> = match s {
            Stmt::Assign { args, .. } => args
                .iter_mut()
                .filter_map(|a| match &mut a.val {
                    ArgVal::Value(v) => Some(v),
                    ArgVal::Label(_) => None,
                })
                .collect(),
            Stmt::Return { values, .. } => values.iter_mut().collect(),
        };
        for v in vals {
            if let Value::Ident(i) = v
                && let Some(new) = params.get(&i.text)
            {
                i.text = new.clone();
            }
        }
    }
    let mut inv_labels: HashMap<String, String> = HashMap::new();
    for c in &mut n.clauses {
        let cost = matches!(c.kind, ClauseKind::Cost | ClauseKind::Budget);
        let mut scope: Vec<(String, String)> = Vec::new();
        if let ClauseKind::Invariant { label, k, acc } = &mut c.kind {
            let nl = fresh(rng, "inv");
            inv_labels.insert(label.text.clone(), nl.clone());
            label.text = nl;
            for b in [k, acc] {
                let nb = fresh(rng, "b");
                scope.push((b.text.clone(), nb.clone()));
                b.text = nb;
            }
        }
        rename_in_formula(&mut c.formula, &params, &mut scope, cost, rng);
    }
    for p in &mut n.params {
        rename_in_type(&mut p.ty, &params, rng);
    }
    for t in &mut n.results {
        rename_in_type(t, &params, rng);
    }
    for s in &mut n.body {
        if let Stmt::Assign { args, .. } = s {
            for a in args {
                if a.name.as_ref().is_some_and(|x| x.text == "invariant")
                    && let ArgVal::Label(l) = &mut a.val
                    && let Some(new) = inv_labels.get(&l.text)
                {
                    l.text = new.clone();
                }
            }
        }
    }
    true
}

fn rename_in_type(t: &mut TypeExpr, params: &HashMap<String, String>, rng: &mut Rng) {
    if let TypeExpr::Refine(v, _, f) = t {
        let nv = fresh(rng, "r");
        let mut scope = vec![(v.text.clone(), nv.clone())];
        v.text = nv;
        rename_in_formula(f, params, &mut scope, false, rng);
    }
}

fn rename_in_formula(
    f: &mut Formula,
    params: &HashMap<String, String>,
    scope: &mut Vec<(String, String)>,
    cost: bool,
    rng: &mut Rng,
) {
    match &mut f.kind {
        FKind::Value(Value::Ident(i)) => {
            if let Some((_, new)) = scope.iter().rev().find(|(o, _)| *o == i.text) {
                i.text = new.clone();
            } else if cost && RESOURCES.contains(&i.text.as_str()) {
            } else if let Some(new) = params.get(&i.text) {
                i.text = new.clone();
            }
        }
        FKind::Forall { ranges, body } => {
            let depth = scope.len();
            for r in ranges.iter_mut() {
                rename_in_formula(&mut r.lo, params, scope, cost, rng);
                rename_in_formula(&mut r.hi, params, scope, cost, rng);
                let nv = fresh(rng, "v");
                scope.push((r.var.text.clone(), nv.clone()));
                r.var.text = nv;
            }
            rename_in_formula(body, params, scope, cost, rng);
            scope.truncate(depth);
        }
        FKind::Call { args, .. } => args
            .iter_mut()
            .for_each(|a| rename_in_formula(a, params, scope, cost, rng)),
        FKind::Unary(_, a) | FKind::Proj(a, _) => rename_in_formula(a, params, scope, cost, rng),
        FKind::Binary(_, a, b) => {
            rename_in_formula(a, params, scope, cost, rng);
            rename_in_formula(b, params, scope, cost, rng);
        }
        FKind::Index(a, idx) => {
            rename_in_formula(a, params, scope, cost, rng);
            idx.iter_mut()
                .for_each(|i| rename_in_formula(i, params, scope, cost, rng));
        }
        _ => {}
    }
}

/// Renames one NODE, SPEC or query label everywhere in the module.
fn rename_label(m: &mut Module, rng: &mut Rng) -> bool {
    let labels: Vec<String> = m
        .items
        .iter()
        .map(|it| match it {
            Item::Node(n) => n.label.clone(),
            Item::Query(q) => q.label.clone(),
        })
        .collect();
    if labels.is_empty() {
        return false;
    }
    let old = labels[rng.below(labels.len())].clone();
    let new = fresh(rng, "lbl");
    for it in &mut m.items {
        match it {
            Item::Query(q) => {
                if q.label == old {
                    q.label = new.clone();
                }
            }
            Item::Node(n) => {
                if n.label == old {
                    n.label = new.clone();
                }
                for l in n.learnable.iter_mut().flatten() {
                    if let Some(loss) = &mut l.loss
                        && loss.text == old
                    {
                        loss.text = new.clone();
                    }
                }
                for s in &mut n.body {
                    if let Stmt::Assign { args, .. } = s {
                        for a in args {
                            let invariant = a.name.as_ref().is_some_and(|x| x.text == "invariant");
                            if let ArgVal::Label(l) = &mut a.val
                                && !invariant
                                && l.text == old
                            {
                                l.text = new.clone();
                            }
                        }
                    }
                }
                let mut rename = |k: &mut FKind| {
                    if let FKind::Call { name, .. } = k
                        && name.text == old
                    {
                        name.text = new.clone();
                    }
                };
                for c in &mut n.clauses {
                    walk_formula(&mut c.formula, &mut rename);
                }
                for t in n
                    .params
                    .iter_mut()
                    .map(|p| &mut p.ty)
                    .chain(n.results.iter_mut())
                {
                    if let TypeExpr::Refine(_, _, f) = t {
                        walk_formula(f, &mut rename);
                    }
                }
            }
        }
    }
    true
}

fn change_intent(m: &mut Module, ni: usize, rng: &mut Rng) -> bool {
    let n = node_mut(m, ni);
    let text = format!("\"variant {}\"", rng.next_u64() % 1000);
    match n.attrs.iter_mut().find(|a| matches!(a, Attr::Intent(_))) {
        Some(a) => *a = Attr::Intent(text),
        None if !n.spec => n.attrs.push(Attr::Intent(text)),
        None => return false,
    }
    true
}

fn defined(s: &Stmt) -> Vec<String> {
    match s {
        Stmt::Assign { binds, .. } => binds.iter().map(|b| b.port.text.clone()).collect(),
        Stmt::Return { .. } => vec![],
    }
}

fn used(s: &Stmt) -> Vec<String> {
    let mut s = s.clone();
    port_uses(&mut s)
        .into_iter()
        .map(|p| p.text.clone())
        .collect()
}

/// A random topological order of the statements; RETURN stays last.
fn reorder_stmts(m: &mut Module, ni: usize, rng: &mut Rng) -> bool {
    let n = node_mut(m, ni);
    let body = std::mem::take(&mut n.body);
    let (ret, stmts) = body.split_last().expect("BODY is not empty");
    let def_at: HashMap<String, usize> = stmts
        .iter()
        .enumerate()
        .flat_map(|(i, s)| defined(s).into_iter().map(move |p| (p, i)))
        .collect();
    let deps: Vec<Vec<usize>> = stmts
        .iter()
        .map(|s| {
            used(s)
                .iter()
                .filter_map(|p| def_at.get(p).copied())
                .collect()
        })
        .collect();
    let mut placed = vec![false; stmts.len()];
    let mut order = Vec::new();
    while order.len() < stmts.len() {
        let ready: Vec<usize> = (0..stmts.len())
            .filter(|&i| !placed[i] && deps[i].iter().all(|&d| placed[d]))
            .collect();
        let pick = ready[rng.below(ready.len())];
        placed[pick] = true;
        order.push(pick);
    }
    n.body = order
        .into_iter()
        .map(|i| stmts[i].clone())
        .chain(std::iter::once(ret.clone()))
        .collect();
    true
}

/// Copies a pure single-result statement under a fresh port, right after it. With `redirect`, some
/// later uses move to the copy; without, the copy is dead code.
fn duplicate_op(
    m: &mut Module,
    ni: usize,
    rng: &mut Rng,
    effectful: &HashSet<String>,
    redirect: bool,
) -> bool {
    let n = node_mut(m, ni);
    let candidates: Vec<usize> = n
        .body
        .iter()
        .enumerate()
        .filter(|(_, s)| match s {
            Stmt::Assign {
                binds, op, args, ..
            } => {
                binds.len() == 1
                    && pure_stmt(&op.text)
                    && !args
                        .iter()
                        .any(|a| matches!(&a.val, ArgVal::Label(l) if effectful.contains(&l.text)))
            }
            Stmt::Return { .. } => false,
        })
        .map(|(i, _)| i)
        .collect();
    if candidates.is_empty() {
        return false;
    }
    let i = candidates[rng.below(candidates.len())];
    let mut copy = n.body[i].clone();
    let Stmt::Assign { binds, .. } = &mut copy else {
        unreachable!()
    };
    let old = binds[0].port.text.clone();
    let new = (20000 + rng.below(10000)).to_string();
    if n.body.iter().any(|s| defined(s).contains(&new)) {
        return false;
    }
    binds[0].port.text = new.clone();
    n.body.insert(i + 1, copy);
    if redirect {
        for s in &mut n.body[i + 2..] {
            for p in port_uses(s) {
                if p.text == old && rng.chance(1, 2) {
                    p.text = new.clone();
                }
            }
        }
    }
    true
}

fn swap_commutative(m: &mut Module, ni: usize, rng: &mut Rng) -> bool {
    let n = node_mut(m, ni);
    let candidates: Vec<usize> = n
        .body
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, Stmt::Assign { op, args, .. } if COMMUTATIVE.contains(&op.text.as_str()) && args.len() >= 2))
        .map(|(i, _)| i)
        .collect();
    if candidates.is_empty() {
        return false;
    }
    if let Stmt::Assign { args, .. } = &mut n.body[candidates[rng.below(candidates.len())]] {
        args.swap(0, 1);
    }
    true
}

/// Replaces one value argument by a fresh port bound to it through IDENTITY just before.
fn bind_identity(m: &mut Module, ni: usize, rng: &mut Rng) -> bool {
    let n = node_mut(m, ni);
    let mut slots: Vec<(usize, usize)> = Vec::new();
    for (i, s) in n.body.iter().enumerate() {
        match s {
            Stmt::Assign { args, .. } => {
                for (j, a) in args.iter().enumerate() {
                    if matches!(a.val, ArgVal::Value(_)) {
                        slots.push((i, j));
                    }
                }
            }
            Stmt::Return { values, .. } => slots.extend((0..values.len()).map(|j| (i, j))),
        }
    }
    if slots.is_empty() {
        return false;
    }
    let (i, j) = slots[rng.below(slots.len())];
    let port = (30000 + rng.below(10000)).to_string();
    if n.body.iter().any(|s| defined(s).contains(&port)) {
        return false;
    }
    let new = Value::Port(Name::new(port.clone()));
    let old = match &mut n.body[i] {
        Stmt::Assign { args, .. } => match &mut args[j].val {
            ArgVal::Value(v) => std::mem::replace(v, new),
            ArgVal::Label(_) => unreachable!("a value slot"),
        },
        Stmt::Return { values, .. } => std::mem::replace(&mut values[j], new),
    };
    let stmt = Stmt::Assign {
        binds: vec![Bind {
            port: Name::new(port),
            ty: None,
        }],
        op: Name::new("IDENTITY"),
        mode: None,
        args: vec![Arg {
            name: None,
            val: ArgVal::Value(old),
            span: Default::default(),
        }],
        span: Default::default(),
    };
    n.body.insert(i, stmt);
    true
}

/// Splits a clause at its top-level `&&` into two clauses of the same kind, or moves a clause
/// past one of another kind; both keep each kind's conjuncts in order.
fn split_or_move_clauses(m: &mut Module, ni: usize, rng: &mut Rng) -> bool {
    let n = node_mut(m, ni);
    if n.clauses.is_empty() {
        return false;
    }
    let i = rng.below(n.clauses.len());
    if rng.chance(1, 2) {
        let c = &n.clauses[i];
        if let (FKind::Binary(BinOp::And, a, b), false) = (
            &c.formula.kind,
            matches!(c.kind, ClauseKind::Invariant { .. }),
        ) {
            let (first, second) = ((**a).clone(), (**b).clone());
            let kind = c.kind.clone();
            n.clauses[i].formula = first;
            n.clauses.insert(
                i + 1,
                Clause {
                    kind,
                    formula: second,
                    span: Default::default(),
                },
            );
            return true;
        }
    }
    let same =
        |a: &ClauseKind, b: &ClauseKind| std::mem::discriminant(a) == std::mem::discriminant(b);
    if i + 1 < n.clauses.len()
        && (!same(&n.clauses[i].kind, &n.clauses[i + 1].kind)
            || matches!(n.clauses[i].kind, ClauseKind::Invariant { .. }))
    {
        n.clauses.swap(i, i + 1);
        return true;
    }
    false
}

/// Re-lays out printed VG-Sym: every run of whitespace between tokens becomes spaces, tabs,
/// newlines or a comment, chosen at random. Adjacent tokens stay adjacent.
pub fn layout(text: &str, seed: u64) -> String {
    let mut rng = Rng(seed ^ 0x1a70_u64);
    let chars: Vec<char> = text.chars().collect();
    let toks = lexer::lex(text).expect("printed VG-Sym lexes");
    let mut out = String::new();
    let mut at = 0usize;
    for t in &toks {
        let start = t.span.offset as usize;
        let gap: String = chars[at..start].iter().collect();
        if !gap.is_empty() {
            out.push_str(match rng.below(6) {
                0 => " ",
                1 => "\n",
                2 => "\t",
                3 => "   ",
                4 => " // moved\n  ",
                _ => "\n\n",
            });
        }
        out.extend(&chars[start..start + t.span.len as usize]);
        at = start + t.span.len as usize;
    }
    out.push('\n');
    out
}

/// The two program classes of docs/16, "Differential testing".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Total ops only and a `true` contract: wrap and sat arithmetic, divisors made non-zero, every
    /// float op with safe bounds and signs, comparisons, SELECT, widening casts, indexing reduced
    /// modulo the length.
    A,
    /// Class A plus checked arithmetic made safe by a generated PRE, and SIMD_MAP and FOLD_BOUNDED
    /// over generated kernels.
    B,
}

const INTS: &[&str] = &["u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64"];
const FLOATS: &[&str] = &["f32", "f64"];

fn bits(t: &str) -> u32 {
    t[1..].parse().expect("an int or float keyword")
}

fn signed(t: &str) -> bool {
    t.starts_with('i')
}

fn int_range(t: &str) -> (i128, i128) {
    let b = bits(t);
    if signed(t) {
        (-(1i128 << (b - 1)), (1i128 << (b - 1)) - 1)
    } else {
        (0, (1i128 << b) - 1)
    }
}

/// A value in reach: its VG-Sym text, its type, and an interval for integers.
type Pooled = (String, String, Option<(i128, i128)>);

struct Gen {
    rng: Rng,
    /// Values in reach: (VG-Sym text, type, interval for integers).
    pool: Vec<Pooled>,
    body: Vec<String>,
    next_port: usize,
}

impl Gen {
    fn port(&mut self) -> String {
        let p = format!("%{}", self.next_port);
        self.next_port += 1;
        p
    }

    fn literal(&mut self, t: &str) -> (String, Option<(i128, i128)>) {
        match t {
            "Bool" => (
                (if self.rng.chance(1, 2) {
                    "true"
                } else {
                    "false"
                })
                .into(),
                None,
            ),
            "f32" | "f64" => {
                const V: &[&str] = &[
                    "0.0", "-0.0", "1.0", "-1.5", "0.25", "2.0", "1.0e-30", "3.0e38", "-7.75",
                    "100.0",
                ];
                let v = V[self.rng.below(V.len())];
                (format!("{v}:{t}"), None)
            }
            _ => {
                let (lo, hi) = int_range(t);
                let v = match self.rng.below(6) {
                    0 => 0,
                    1 => 1,
                    2 => hi,
                    3 => lo,
                    _ => (self.rng.next_u64() % 200) as i128 + lo.max(-100),
                }
                .clamp(lo, hi);
                (format!("{v}:{t}"), Some((v, v)))
            }
        }
    }

    /// A value of type t: from the pool, or a fresh literal.
    fn operand(&mut self, t: &str) -> (String, Option<(i128, i128)>) {
        let have: Vec<usize> = (0..self.pool.len())
            .filter(|&i| self.pool[i].1 == t)
            .collect();
        if have.is_empty() || self.rng.chance(1, 4) {
            return self.literal(t);
        }
        let (v, _, r) = &self.pool[have[self.rng.below(have.len())]];
        (v.clone(), *r)
    }

    fn emit(&mut self, t: &str, range: Option<(i128, i128)>, op: String) {
        let p = self.port();
        self.body.push(format!("{p} = {op};"));
        self.pool.push((p, t.to_string(), range));
    }

    fn pick_type(&mut self) -> String {
        let types: Vec<String> = self.pool.iter().map(|p| p.1.clone()).collect();
        types[self.rng.below(types.len())].clone()
    }

    fn step(&mut self, class: Class) {
        let t = self.pick_type();
        if t == "Bool" {
            let (a, _) = self.operand("Bool");
            let (b, _) = self.operand("Bool");
            let op = match self.rng.below(4) {
                0 => format!("BAND({a}, {b})"),
                1 => format!("BOR({a}, {b})"),
                2 => format!("BNOT({a})"),
                _ => format!("EQ({a}, {b})"),
            };
            return self.emit("Bool", None, op);
        }
        if t.starts_with("Array") {
            // Indexing reduced modulo the length: INDEX(xs, MOD.wrap(i, n)).
            let (elem, n) = t["Array<".len()..t.len() - 1]
                .split_once(", ")
                .expect("Array<T, N>");
            let (elem, n) = (elem.to_string(), n.to_string());
            let arrays: Vec<String> = self
                .pool
                .iter()
                .filter(|p| p.1 == t)
                .map(|p| p.0.clone())
                .collect();
            let xs = arrays[self.rng.below(arrays.len())].clone();
            let (i, _) = self.operand("u64");
            let m = self.port();
            self.body.push(format!("{m} = MOD.wrap({i}, {n}:u64);"));
            return self.emit(&elem, None, format!("INDEX({xs}, {m})"));
        }
        if FLOATS.contains(&t.as_str()) {
            let (a, _) = self.operand(&t);
            let (b, _) = self.operand(&t);
            let (c, _) = self.operand(&t);
            let op = match self.rng.below(16) {
                0 => format!("FADD({a}, {b})"),
                1 => format!("FSUB({a}, {b})"),
                2 => format!("FMUL({a}, {b})"),
                3 => format!("FDIV({a}, {b})"),
                4 => format!("FMA({a}, {b}, {c})"),
                5 => format!("FNEG({a})"),
                6 => format!("FABS({a})"),
                7 => format!("FMIN({a}, {b})"),
                8 => format!("FMAX({a}, {b})"),
                9 => format!("FSQRT({a})"),
                10 => format!("FCLAMP({a}, -1.0:{t}, 2.5:{t})"),
                11 => format!("FCOPYSIGN({a}, -1.0:{t})"),
                12 => format!("SELECT({}, {a}, {b})", self.operand("Bool").0),
                13 if t == "f32" => return self.emit("f64", None, format!("CAST.f64({a})")),
                _ => {
                    let cmp = ["LT", "LE", "GT", "GE"][self.rng.below(4)];
                    return self.emit("Bool", None, format!("{cmp}.f({a}, {b})"));
                }
            };
            return self.emit(&t, None, op);
        }
        // Integers.
        let (a, ra) = self.operand(&t);
        let (b, rb) = self.operand(&t);
        let (lo, hi) = int_range(&t);
        let mode = if self.rng.chance(1, 2) { "wrap" } else { "sat" };
        let k = self.rng.below(15);
        // Class B: checked arithmetic when the operands' intervals prove it safe.
        if class == Class::B
            && k < 3
            && let (Some((alo, ahi)), Some((blo, bhi))) = (ra, rb)
        {
            {
                let corners = |f: fn(i128, i128) -> i128| {
                    let v = [f(alo, blo), f(alo, bhi), f(ahi, blo), f(ahi, bhi)];
                    (*v.iter().min().unwrap(), *v.iter().max().unwrap())
                };
                let (name, r) = match k {
                    0 => ("ADD", corners(i128::saturating_add)),
                    1 => ("SUB", corners(i128::saturating_sub)),
                    _ => ("MUL", corners(i128::saturating_mul)),
                };
                if r.0 >= lo && r.1 <= hi {
                    return self.emit(&t, Some(r), format!("{name}.checked({a}, {b})"));
                }
            }
        }
        let op = match k {
            0 => format!("ADD.{mode}({a}, {b})"),
            1 => format!("SUB.{mode}({a}, {b})"),
            2 => format!("MUL.{mode}({a}, {b})"),
            3 => format!("NEG.{mode}({a})"),
            4 | 5 => {
                // A divisor made non-zero by construction.
                let nz = self.port();
                self.body.push(format!("{nz} = OR({b}, 1:{t});"));
                format!("{}.{mode}({a}, {nz})", if k == 4 { "DIV" } else { "MOD" })
            }
            6 => format!("AND({a}, {b})"),
            7 => format!("OR({a}, {b})"),
            8 => format!("XOR({a}, {b})"),
            9 => format!("NOT({a})"),
            10 => format!(
                "{}.wrap({a}, {})",
                ["SHL", "SHR"][self.rng.below(2)],
                self.operand("u32").0
            ),
            11 => format!(
                "{}({a}, {})",
                ["ROTL", "ROTR"][self.rng.below(2)],
                self.operand("u32").0
            ),
            12 => {
                let wider: Vec<&str> = INTS
                    .iter()
                    .copied()
                    .filter(|w| bits(w) > bits(&t) && (signed(w) || !signed(&t)))
                    .chain(["f64"])
                    .collect();
                let w = wider[self.rng.below(wider.len())];
                return self.emit(w, ra.filter(|_| w != "f64"), format!("CAST.{w}({a})"));
            }
            13 => format!("SELECT({}, {a}, {b})", self.operand("Bool").0),
            _ => {
                let cmp = ["EQ", "NE", "LT", "LE", "GT", "GE"][self.rng.below(6)];
                let m = if cmp.len() == 2 && cmp.starts_with(['E', 'N']) {
                    String::new()
                } else {
                    format!(".{}", if signed(&t) { "s" } else { "u" })
                };
                return self.emit("Bool", None, format!("{cmp}{m}({a}, {b})"));
            }
        };
        self.emit(&t, None, op);
    }
}

/// A random, well-typed module from a seed, with `ops` statements or a few more: docs/16's
/// classes A and B. Every program assembles; class A needs no proof beyond its construction.
pub fn program(seed: u64, class: Class, ops: usize) -> String {
    let mut g = Gen {
        rng: Rng(seed),
        pool: Vec::new(),
        body: Vec::new(),
        next_port: 0,
    };
    let mut params = Vec::new();
    let mut pre = Vec::new();
    let all: Vec<&str> = INTS.iter().chain(FLOATS).copied().chain(["Bool"]).collect();
    for i in 0..1 + g.rng.below(4) {
        let t = all[g.rng.below(all.len())];
        let name = format!("x{i}");
        let range = if INTS.contains(&t) {
            if class == Class::B {
                let r = [10i128, 100, 1000][g.rng.below(3)].min(int_range(t).1);
                pre.push(format!("{name} <= {r}"));
                if signed(t) {
                    pre.push(format!("{name} >= -{r}"));
                    Some((-r, r))
                } else {
                    Some((0, r))
                }
            } else {
                Some(int_range(t))
            }
        } else {
            None
        };
        params.push(format!("{name}: {t}"));
        g.pool.push((name, t.to_string(), range));
    }
    let mut kernels = String::new();
    let mut inv = String::new();
    if g.rng.chance(1, 2) {
        let e = INTS[g.rng.below(INTS.len())];
        let n = 1 + g.rng.below(8);
        params.push(format!("xs: Array<{e}, {n}>"));
        g.pool.push(("xs".into(), format!("Array<{e}, {n}>"), None));
        if class == Class::B {
            kernels = format!(
                "NODE @k0 {{ SIGNATURE: (v: {e}) -> ({e}); EFFECTS: []; CONTRACT {{ }} BODY {{ %0 = ADD.wrap(v, 3:{e}); RETURN(%0); }} }}\n\
                 NODE @f0 {{ SIGNATURE: (acc: {e}, v: {e}) -> ({e}); EFFECTS: []; CONTRACT {{ }} BODY {{ %0 = XOR(acc, v); RETURN(%0); }} }}\n"
            );
            inv = format!("    INVARIANT @inv0(k, acc): k <= {n};\n");
            let m = g.port();
            g.body.push(format!("{m} = SIMD_MAP(@k0, xs);"));
            g.pool.push((m, format!("Array<{e}, {n}>"), None));
            let f = g.port();
            g.body.push(format!(
                "{f} = FOLD_BOUNDED(@f0, xs, init=0:{e}, invariant=@inv0);"
            ));
            g.pool.push((f, e.to_string(), None));
        }
    }
    for _ in 0..ops {
        g.step(class);
    }
    // Outputs: one to three values, later ones more likely.
    let n_out = 1 + g.rng.below(3);
    let mut outs: Vec<(String, String)> = Vec::new();
    for _ in 0..n_out {
        let k = g.pool.len();
        let i = k - 1 - g.rng.below(k.min(6));
        outs.push((g.pool[i].0.clone(), g.pool[i].1.clone()));
    }
    let pre = if pre.is_empty() {
        String::new()
    } else {
        format!("    PRE: {};\n", pre.join(" && "))
    };
    format!(
        "module gen;\n{kernels}NODE @root {{\n  SIGNATURE: ({}) -> ({});\n  EFFECTS: [];\n  CONTRACT {{\n{pre}{inv}  }}\n  BODY {{\n    {}\n    RETURN({});\n  }}\n}}\n",
        params.join(", "),
        outs.iter()
            .map(|o| o.1.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        g.body.join("\n    "),
        outs.iter()
            .map(|o| o.0.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}
