//! Source-map recovery and human/agent-readable formatting for VG-IR nodes, ports, ops, and clauses.

use std::collections::{BTreeMap, HashMap};

use vg_core::ast::{self, ArgVal, Bind, ClauseKind, FKind, Item, Module, NodeDef, Stmt, TypeExpr};
use vg_core::canon::{children, term_head};
use vg_core::diag::Diagnostic;
use vg_core::hash::{Context, derive};
use vg_core::ir::{Bin, Const, Hash, Node, Operand, Term};
use vg_core::lower::{Callable, Kind, Labels, int_value, lower_type};
use vg_core::opcode::{self, Comm, Modes, Shape};
use vg_core::printer;
use vg_core::types::{EffectMask, Type};
use vg_core::{MeshLookup, parser};

use crate::trace::ObligationKind;

/// Maps canonical VG-IR positions back to the names and syntax written by the author.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    /// Input names (parameters followed by LEARNABLE θ), indexed by canonical input index.
    pub params: Vec<String>,
    /// Written port name (without `%`) for canonical `(op_idx, slot)`.
    pub ports: BTreeMap<(u32, u8), String>,
    /// Formatted op RHS as written (e.g. `"FDIV(%2, %3)"`) for canonical `op_idx`.
    pub op_text: BTreeMap<u32, String>,
    /// Callee hash -> label without `@`.
    pub callees: BTreeMap<Hash, String>,
    /// Canonical invariant index -> `(label without @, k_name, acc_name)`.
    pub invariants: BTreeMap<u32, (String, String, String)>,
    /// Flattened top-level conjuncts as written for PRE (including param refinements in order).
    pub pre_clauses: Vec<String>,
    /// Flattened top-level conjuncts as written for POST (including result refinements in order).
    pub post_clauses: Vec<String>,
    /// Flattened top-level conjuncts as written for COST.
    pub cost_clauses: Vec<String>,
    /// Canonical invariant index -> flattened conjunct strings.
    pub invariant_clauses: BTreeMap<u32, Vec<String>>,
}

impl Names {
    /// Builds canonical fallback names from a `Node` alone, using `node.debug` if present to
    /// recover parameter names and clause strings.
    pub fn from_node(node: &Node) -> Self {
        let mut params = Vec::with_capacity(node.input_count());
        for i in 0..node.params.len() {
            params.push(format!("a{i}"));
        }
        for j in 0..node.learnable.len() {
            params.push(format!("t{j}"));
        }

        let mut ports = BTreeMap::new();
        let mut next_port = 0u32;
        for (i, op) in node.ops.iter().enumerate() {
            for s in 0..op.results.len() {
                ports.insert((i as u32, s as u8), next_port.to_string());
                next_port += 1;
            }
        }

        let mut callees = BTreeMap::new();
        for h in &node.calls {
            let hx = vg_core::ir::hex(h);
            callees.insert(*h, format!("n{}", &hx[..12]));
        }

        let mut invariants = BTreeMap::new();
        for v in 0..node.contract.invariants.len() as u32 {
            invariants.insert(v, (format!("i{v}"), "v0".to_string(), "v1".to_string()));
        }

        let mut names = Self {
            params,
            ports,
            op_text: BTreeMap::new(),
            callees,
            invariants,
            pre_clauses: Vec::new(),
            post_clauses: Vec::new(),
            cost_clauses: Vec::new(),
            invariant_clauses: BTreeMap::new(),
        };

        if let Some(debug_src) = &node.debug {
            let wrapped = format!("module _debug;\n{debug_src}");
            if let Ok(ast) = parser::parse(&wrapped)
                && let Some(Item::Node(def)) = ast.items.into_iter().next()
            {
                names.apply_def_metadata(node, &def);
            }
        }
        names
    }

    /// Builds `Names` by parsing `src` and resolving the named node `label` (with or without `@`).
    pub fn from_source(src: &str, label: &str, mesh: MeshLookup) -> Result<Self, Diagnostic> {
        let ast = parser::parse(src)?;
        Self::from_module(&ast, label, mesh)
    }

    /// Builds `Names` from an already-parsed AST module for node `label`.
    pub fn from_module(module: &Module, label: &str, mesh: MeshLookup) -> Result<Self, Diagnostic> {
        let target_label = label.trim_start_matches('@');
        let assembled = vg_core::asm_module(module, "", mesh, false)?;

        let mut callee_labels: BTreeMap<Hash, String> = BTreeMap::new();
        let mut labels_map: Labels = HashMap::new();
        let mut target_item: Option<(usize, &NodeDef)> = None;

        for (k, item) in module.items.iter().enumerate() {
            match item {
                Item::Node(n) => {
                    let mut effects = 0;
                    for e in &n.effects {
                        effects |= EffectMask::bit(&e.text).unwrap_or(0);
                    }
                    let base = |t: &TypeExpr, span| match t {
                        TypeExpr::Refine(_, b, _) => lower_type(b, span),
                        t => lower_type(t, span),
                    };
                    let asm = assembled.iter().find(|a| a.label == n.label);
                    let hash = asm.map(|a| a.hashes.node);
                    if let Some(h) = hash {
                        callee_labels.insert(h, n.label.clone());
                    }
                    if n.label == target_label {
                        target_item = Some((k, n));
                    }
                    let c = Callable {
                        kind: if n.spec { Kind::Spec } else { Kind::Node },
                        item: k,
                        params: n
                            .params
                            .iter()
                            .map(|p| base(&p.ty, p.name.span))
                            .collect::<Result<_, _>>()?,
                        results: n
                            .results
                            .iter()
                            .map(|t| base(t, n.span))
                            .collect::<Result<_, _>>()?,
                        effects: EffectMask(effects),
                        hash,
                        spec: if n.spec { hash } else { None },
                    };
                    labels_map.insert(n.label.clone(), c);
                }
                Item::Query(q) => {
                    let mut effects = 0;
                    for e in &q.effects {
                        effects |= EffectMask::bit(&e.text).unwrap_or(0);
                    }
                    let resolved = mesh(&q.label);
                    if let Some((h, _)) = resolved {
                        callee_labels.insert(h, q.label.clone());
                    }
                    let c = Callable {
                        kind: Kind::Query,
                        item: k,
                        params: q
                            .params
                            .iter()
                            .map(|t| lower_type(t, q.span))
                            .collect::<Result<_, _>>()?,
                        results: q
                            .results
                            .iter()
                            .map(|t| lower_type(t, q.span))
                            .collect::<Result<_, _>>()?,
                        effects: EffectMask(effects),
                        hash: resolved.map(|r| r.0),
                        spec: resolved.and_then(|r| r.1),
                    };
                    labels_map.insert(q.label.clone(), c);
                }
            }
        }

        let (item_idx, def) = target_item.ok_or_else(|| {
            Diagnostic::error(
                "VG-S0005",
                Default::default(),
                format!("unknown label @{target_label}"),
            )
        })?;
        let canonical = assembled
            .iter()
            .find(|a| a.label == target_label)
            .map(|a| &a.node)
            .ok_or_else(|| {
                Diagnostic::error(
                    "VG-S0005",
                    def.span,
                    format!("missing assembled node @{target_label}"),
                )
            })?;

        let mut names = Self::from_node(canonical);
        for (h, lbl) in &callee_labels {
            names.callees.insert(*h, lbl.clone());
        }
        names.apply_def_metadata(canonical, def);
        names.map_ops_from_def(canonical, def, &labels_map, item_idx)?;
        Ok(names)
    }

    /// Convenience constructor from a `Node` and its `NodeDef` plus known callee labels.
    pub fn from_def(node: &Node, def: &NodeDef, callee_labels: BTreeMap<Hash, String>) -> Self {
        let mut names = Self::from_node(node);
        for (h, lbl) in &callee_labels {
            names.callees.insert(*h, lbl.clone());
        }
        names.apply_def_metadata(node, def);

        // Build a synthetic Labels table for op matching using callee_labels.
        let mut labels_map: Labels = HashMap::new();
        for (lbl, h) in callee_labels.iter().map(|(h, l)| (l.as_str(), *h)) {
            labels_map.insert(
                lbl.to_string(),
                Callable {
                    kind: Kind::Node,
                    item: 0,
                    params: Vec::new(),
                    results: Vec::new(),
                    effects: EffectMask(0),
                    hash: Some(h),
                    spec: Some(h),
                },
            );
        }
        let _ = names.map_ops_from_def(node, def, &labels_map, 1);
        names
    }

    fn apply_def_metadata(&mut self, node: &Node, def: &NodeDef) {
        let mut params = Vec::with_capacity(node.input_count());
        for p in &def.params {
            params.push(p.name.text.clone());
        }
        for l in def.learnable.iter().flatten() {
            params.push(l.name.text.clone());
        }
        if params.len() == node.input_count() {
            self.params = params;
        }

        // Flatten clauses in the exact order of vg_core::lower::NodeLowering::contract:
        // explicit PREs, then parameter refinements in parameter order;
        // explicit POSTs, then result refinements in result order;
        // COSTs in source order.
        let mut pre_clauses = Vec::new();
        let mut post_clauses = Vec::new();
        let mut cost_clauses = Vec::new();
        let mut src_invs: Vec<(String, String, String, Vec<String>)> = Vec::new();

        for c in &def.clauses {
            match &c.kind {
                ClauseKind::Pre => flatten_ast_and(&c.formula, &mut pre_clauses),
                ClauseKind::Post => flatten_ast_and(&c.formula, &mut post_clauses),
                ClauseKind::Cost => flatten_ast_and(&c.formula, &mut cost_clauses),
                ClauseKind::Invariant { label, k, acc } => {
                    let mut conj = Vec::new();
                    flatten_ast_and(&c.formula, &mut conj);
                    src_invs.push((label.text.clone(), k.text.clone(), acc.text.clone(), conj));
                }
                ClauseKind::Budget | ClauseKind::Decreases => {}
            }
        }
        for p in &def.params {
            if let TypeExpr::Refine(var, _, formula) = &p.ty {
                let renamed = substitute_ident(formula, &var.text, &p.name.text);
                flatten_ast_and(&renamed, &mut pre_clauses);
            }
        }
        for (j, r) in def.results.iter().enumerate() {
            if let TypeExpr::Refine(var, _, formula) = r {
                let target = if def.results.len() == 1 {
                    ast::Formula::new(FKind::Result)
                } else {
                    ast::Formula::new(FKind::Proj(
                        Box::new(ast::Formula::new(FKind::Result)),
                        j.to_string(),
                    ))
                };
                let renamed = substitute_ident_with_formula(formula, &var.text, &target);
                flatten_ast_and(&renamed, &mut post_clauses);
            }
        }

        if pre_clauses.len() == node.contract.pre.len() {
            self.pre_clauses = pre_clauses;
        }
        if post_clauses.len() == node.contract.post.len() {
            self.post_clauses = post_clauses;
        }
        if cost_clauses.len() == node.contract.cost.len() {
            self.cost_clauses = cost_clauses;
        }
        if src_invs.len() == node.contract.invariants.len() {
            for (idx, (lbl, k, acc, conj)) in src_invs.into_iter().enumerate() {
                self.invariants
                    .entry(idx as u32)
                    .or_insert((lbl.clone(), k.clone(), acc.clone()));
                self.invariant_clauses.entry(idx as u32).or_insert(conj);
            }
        }
    }

    fn map_ops_from_def(
        &mut self,
        canonical: &Node,
        def: &NodeDef,
        labels: &Labels,
        _item_idx: usize,
    ) -> Result<(), Diagnostic> {
        // Reconstruct written statements (skipping IDENTITY) and compute Merkle hashes matching canon.rs.
        let mut param_map: HashMap<String, u32> = HashMap::new();
        for (i, p) in def.params.iter().enumerate() {
            param_map.insert(p.name.text.clone(), i as u32);
        }
        let base_p = def.params.len() as u32;
        for (j, l) in def.learnable.iter().flatten().enumerate() {
            param_map.insert(l.name.text.clone(), base_p + j as u32);
        }

        let mut inv_names: HashMap<String, (u32, String, String, Vec<String>)> = HashMap::new();
        let mut next_inv = 0u32;
        for c in &def.clauses {
            if let ClauseKind::Invariant { label, k, acc } = &c.kind {
                let mut conj = Vec::new();
                flatten_ast_and(&c.formula, &mut conj);
                inv_names.insert(
                    label.text.clone(),
                    (next_inv, k.text.clone(), acc.text.clone(), conj),
                );
                next_inv += 1;
            }
        }

        // Compute canonical Merkle hashes for the canonical node.
        let (canon_hashes, canon_inv_hashes) = compute_node_op_hashes(canonical);

        // Run NodeLowering on def to get lowered ops in exact source order.
        let lowered_res = vg_core::lower::NodeLowering::new(labels, usize::MAX, def).lower();
        if let Ok(lowered) = lowered_res {
            let (l_hashes, l_inv_hashes) = compute_node_op_hashes(&lowered.node);

            // Remap invariants by hash.
            let mut inv_list: Vec<(String, String, String, Vec<String>)> = Vec::new();
            for c in &def.clauses {
                if let ClauseKind::Invariant { label, k, acc } = &c.kind {
                    let mut conj = Vec::new();
                    flatten_ast_and(&c.formula, &mut conj);
                    inv_list.push((label.text.clone(), k.text.clone(), acc.text.clone(), conj));
                }
            }
            for (c_idx, ch) in canon_inv_hashes.iter().enumerate() {
                if let Some(l_idx) = l_inv_hashes.iter().position(|lh| lh == ch)
                    && let Some((lbl, k, acc, conj)) = inv_list.get(l_idx)
                {
                    self.invariants
                        .insert(c_idx as u32, (lbl.clone(), k.clone(), acc.clone()));
                    self.invariant_clauses.insert(c_idx as u32, conj.clone());
                }
            }

            // Collect non-IDENTITY Assign statements in exact lowering order.
            let mut stmt_records: Vec<(Vec<Bind>, String, bool)> = Vec::new();
            for stmt in &def.body {
                if let Stmt::Assign {
                    binds,
                    op,
                    mode,
                    args,
                    ..
                } = stmt
                {
                    if op.text == "IDENTITY" {
                        continue;
                    }
                    let info = opcode::by_name(&op.text);
                    let is_eff = info.is_some_and(|i| i.is_effect());
                    stmt_records.push((
                        binds.clone(),
                        format_ast_assign(op, mode.as_ref(), args),
                        is_eff,
                    ));
                }
            }

            if stmt_records.len() == lowered.node.ops.len() {
                // Match effect ops by order, pure ops by Merkle hash.
                let canon_eff: Vec<usize> = canonical
                    .ops
                    .iter()
                    .enumerate()
                    .filter(|(_, op)| opcode::by_code(op.opcode).is_some_and(|i| i.is_effect()))
                    .map(|(i, _)| i)
                    .collect();
                let src_eff: Vec<usize> = stmt_records
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, _, eff))| *eff)
                    .map(|(i, _)| i)
                    .collect();

                let mut matched_src = vec![None; canonical.ops.len()];
                if canon_eff.len() == src_eff.len() {
                    for (ci, li) in canon_eff.into_iter().zip(src_eff) {
                        matched_src[ci] = Some(li);
                    }
                }
                for (ci, ch) in canon_hashes.iter().enumerate() {
                    if matched_src[ci].is_some() {
                        continue;
                    }
                    if let Some(li) = l_hashes
                        .iter()
                        .enumerate()
                        .position(|(li, lh)| !stmt_records[li].2 && lh == ch)
                    {
                        matched_src[ci] = Some(li);
                    }
                }

                for (ci, li_opt) in matched_src.into_iter().enumerate() {
                    if let Some(li) = li_opt {
                        let (binds, rhs, _) = &stmt_records[li];
                        for (slot, b) in binds.iter().enumerate() {
                            self.ports
                                .insert((ci as u32, slot as u8), b.port.text.clone());
                        }
                        self.op_text.insert(ci as u32, rhs.clone());
                    }
                }
                return Ok(());
            }
        }

        // Fallback if NodeLowering failed (should not happen for valid modules).
        Ok(())
    }

    pub fn input_name(&self, idx: u32) -> String {
        self.params
            .get(idx as usize)
            .cloned()
            .unwrap_or_else(|| format!("a{idx}"))
    }

    pub fn port_name(&self, op_idx: u32, slot: u8) -> String {
        if let Some(p) = self.ports.get(&(op_idx, slot)) {
            format!("%{p}")
        } else {
            format!("%{op_idx}_{slot}")
        }
    }

    pub fn callee_name(&self, node: &Node, callee_idx: u32) -> String {
        if let Some(h) = node.calls.get(callee_idx as usize) {
            if let Some(lbl) = self.callees.get(h) {
                return format!("@{lbl}");
            }
            let hx = vg_core::ir::hex(h);
            return format!("@n{}", &hx[..12]);
        }
        format!("@c{callee_idx}")
    }

    pub fn invariant_name(&self, inv_idx: u32) -> String {
        if let Some((lbl, _, _)) = self.invariants.get(&inv_idx) {
            format!("@{lbl}")
        } else {
            format!("@i{inv_idx}")
        }
    }

    pub fn format_operand(&self, node: &Node, o: Operand) -> String {
        match o {
            Operand::Input(i) => self.input_name(i),
            Operand::Op(i, s) => self.port_name(i, s),
            Operand::Const(c) => format_const(node, c),
            Operand::Callee(c) => self.callee_name(node, c),
            Operand::Invariant(v) => self.invariant_name(v),
        }
    }

    pub fn format_op(&self, node: &Node, op_idx: u32) -> String {
        if let Some(s) = self.op_text.get(&op_idx) {
            return s.clone();
        }
        let Some(op) = node.ops.get(op_idx as usize) else {
            return format!("<invalid op {op_idx}>");
        };
        let info = opcode::by_code(op.opcode);
        let name = info.map_or("UNKNOWN", |i| i.name);
        let mode_str = info.and_then(|i| {
            if i.modes == Modes::Cast {
                op.results.first().and_then(|t| t.scalar_name())
            } else {
                i.mode_name(op.mode).flatten()
            }
        });
        let head = match mode_str {
            Some(m) => format!("{name}.{m}"),
            None => name.to_string(),
        };
        let shape = info.map(|i| i.shape).unwrap_or(Shape::AtLeast(0));
        let k = op.operands.len();
        let mut args = Vec::with_capacity(k);
        for (j, o) in op.operands.iter().enumerate() {
            let v = self.format_operand(node, *o);
            let named = match (shape, j + 1 == k) {
                (Shape::Fold, true) => Some("invariant"),
                (Shape::Fold, false) if j + 2 == k => Some("init"),
                (Shape::Guard, true) => Some("else"),
                _ => None,
            };
            args.push(match named {
                Some(n) => format!("{n}={v}"),
                None => v,
            });
        }
        format!("{head}({})", args.join(", "))
    }

    /// Formats an IR `Term` back into readable VG-Sym using this node's names.
    pub fn format_term(&self, node: &Node, term: &Term, bound: &[String]) -> String {
        format_term_inner(self, node, term, bound, 0)
    }

    /// Returns the human/agent-readable `clause` string for a given `ObligationKind`.
    pub fn clause_text(
        &self,
        node: &Node,
        kind: &ObligationKind,
        callee: &dyn Fn(&Hash) -> Option<Node>,
    ) -> String {
        match kind {
            ObligationKind::Post(j) => self
                .post_clauses
                .get(*j)
                .cloned()
                .or_else(|| {
                    node.contract
                        .post
                        .get(*j)
                        .map(|t| self.format_term(node, t, &[]))
                })
                .unwrap_or_else(|| format!("POST.{j}")),
            ObligationKind::Cost(j) => self
                .cost_clauses
                .get(*j)
                .cloned()
                .or_else(|| {
                    node.contract
                        .cost
                        .get(*j)
                        .map(|t| self.format_term(node, t, &[]))
                })
                .unwrap_or_else(|| format!("COST.{j}")),
            ObligationKind::NonVacuous => "nonvacuous(PRE)".to_string(),
            ObligationKind::PreWd(j, _) => self
                .pre_clauses
                .get(*j)
                .map(|c| format!("well_defined({c})"))
                .or_else(|| {
                    node.contract
                        .pre
                        .get(*j)
                        .map(|t| format!("well_defined({})", self.format_term(node, t, &[])))
                })
                .unwrap_or_else(|| format!("PRE.{j}.wd")),
            ObligationKind::PostWd(j, _) => self
                .post_clauses
                .get(*j)
                .map(|c| format!("well_defined({c})"))
                .or_else(|| {
                    node.contract
                        .post
                        .get(*j)
                        .map(|t| format!("well_defined({})", self.format_term(node, t, &[])))
                })
                .unwrap_or_else(|| format!("POST.{j}.wd")),
            ObligationKind::OpDomain {
                op_idx,
                op_name,
                kind,
            } => {
                if let Some(op) = node.ops.get(*op_idx as usize) {
                    let arg = |slot: usize| {
                        op.operands
                            .get(slot)
                            .map(|o| self.format_operand(node, *o))
                            .unwrap_or_default()
                    };
                    match (op_name.as_str(), kind.as_str()) {
                        ("DIV" | "MOD", "nonzero") => format!("{} != 0", arg(1)),
                        ("INDEX" | "UPDATE", "in_bounds") => {
                            format!("{} < len({})", arg(1), arg(0))
                        }
                        ("SLICE", "in_bounds") => {
                            format!("{} <= {} && {} <= len({})", arg(1), arg(2), arg(2), arg(0))
                        }
                        ("SHL" | "SHR", "amount") => {
                            let w = node
                                .operand_type(op.operands[0])
                                .and_then(|t| match t {
                                    Type::UInt(w) | Type::SInt(w) => Some(*w),
                                    _ => None,
                                })
                                .unwrap_or(32);
                            format!("{} < {w}", arg(1))
                        }
                        ("FCLAMP", "bounds") => {
                            format!(
                                "!is_nan({}) && !is_nan({}) && {} <= {}",
                                arg(1),
                                arg(2),
                                arg(1),
                                arg(2)
                            )
                        }
                        ("FCOPYSIGN", "sign") => format!("!is_nan({})", arg(1)),
                        ("GET_OK", "is_ok") => format!("is_ok({})", arg(0)),
                        ("GET_ERR", "is_err") => format!("!is_ok({})", arg(0)),
                        ("CAST", "in_range") => {
                            let dst = op
                                .results
                                .first()
                                .map_or_else(|| "target".into(), |t| t.to_string());
                            format!("in_range({}, {dst})", arg(0))
                        }
                        _ => format!("{op_name}.{kind}: {}", self.format_op(node, *op_idx)),
                    }
                } else {
                    format!("{op_name}.{kind}")
                }
            }
            ObligationKind::CallPre { op_idx, pre_idx }
            | ObligationKind::MapPre { op_idx, pre_idx }
            | ObligationKind::GenPre { op_idx, pre_idx }
            | ObligationKind::FoldPre { op_idx, pre_idx } => {
                if let Some(op) = node.ops.get(*op_idx as usize)
                    && let Some(Operand::Callee(c)) = op.operands.first()
                    && let Some(h) = node.calls.get(*c as usize)
                    && let Some(target) = callee(h)
                    && let Some(t) = target.contract.pre.get(*pre_idx)
                {
                    let c_names = Self::from_node(&target);
                    return c_names.format_term(&target, t, &[]);
                }
                format!("PRE.{pre_idx}")
            }
            ObligationKind::GuardThenPre { op_idx, pre_idx } => {
                if let Some(op) = node.ops.get(*op_idx as usize)
                    && let Some(Operand::Callee(c)) = op.operands.get(1)
                    && let Some(h) = node.calls.get(*c as usize)
                    && let Some(target) = callee(h)
                    && let Some(t) = target.contract.pre.get(*pre_idx)
                {
                    let c_names = Self::from_node(&target);
                    return c_names.format_term(&target, t, &[]);
                }
                format!("GUARD.then.pre.{pre_idx}")
            }
            ObligationKind::GuardElsePre { op_idx, pre_idx } => {
                if let Some(op) = node.ops.get(*op_idx as usize)
                    && let Some(Operand::Callee(c)) = op.operands.last()
                    && let Some(h) = node.calls.get(*c as usize)
                    && let Some(target) = callee(h)
                    && let Some(t) = target.contract.pre.get(*pre_idx)
                {
                    let c_names = Self::from_node(&target);
                    return c_names.format_term(&target, t, &[]);
                }
                format!("GUARD.else.pre.{pre_idx}")
            }
            ObligationKind::FoldBase { op_idx } | ObligationKind::FoldStep { op_idx } => {
                if let Some(op) = node.ops.get(*op_idx as usize)
                    && let Some(Operand::Invariant(v)) = op.operands.last()
                {
                    if let Some(conj) = self.invariant_clauses.get(v)
                        && !conj.is_empty()
                    {
                        return conj.join(" && ");
                    }
                    if let Some(terms) = node.contract.invariants.get(*v as usize) {
                        let (_, k_name, acc_name) = self
                            .invariants
                            .get(v)
                            .cloned()
                            .unwrap_or_else(|| (format!("i{v}"), "k".into(), "acc".into()));
                        let bound = vec![k_name, acc_name];
                        let parts: Vec<String> = terms
                            .iter()
                            .map(|t| self.format_term(node, t, &bound))
                            .collect();
                        if !parts.is_empty() {
                            return parts.join(" && ");
                        }
                    }
                }
                self.format_op(node, *op_idx)
            }
            ObligationKind::AssertGuard { op_idx } => {
                if let Some(op) = node.ops.get(*op_idx as usize)
                    && let Some(cond) = op.operands.first()
                {
                    return self.format_operand(node, *cond);
                }
                format!("ASSERT_GUARD@%{op_idx}")
            }
        }
    }
}

fn format_ast_assign(op: &ast::Name, mode: Option<&ast::Name>, args: &[ast::Arg]) -> String {
    let head = match mode {
        Some(m) => format!("{}.{}", op.text, m.text),
        None => op.text.clone(),
    };
    let arg_strs: Vec<String> = args
        .iter()
        .map(|a| {
            let v = match &a.val {
                ArgVal::Value(v) => printer::value(v),
                ArgVal::Label(l) => format!("@{}", l.text),
            };
            match &a.name {
                Some(n) => format!("{}={v}", n.text),
                None => v,
            }
        })
        .collect();
    format!("{head}({})", arg_strs.join(", "))
}

fn format_const(node: &Node, c_idx: u32) -> String {
    let Some(c) = node.consts.get(c_idx as usize) else {
        return format!("const#{c_idx}");
    };
    if c.ty == Type::Int {
        return int_value(&c.bytes).map_or_else(|| format!("const#{c_idx}"), |v| v.to_string());
    }
    match vg_core::disasm::literal(c) {
        Ok(lit) => printer::formula(&ast::Formula::new(FKind::Value(ast::Value::Lit(
            lit,
            Default::default(),
        )))),
        Err(_) => format!("const#{c_idx}"),
    }
}

fn flatten_ast_and(f: &ast::Formula, out: &mut Vec<String>) {
    match &f.kind {
        FKind::Binary(ast::BinOp::And, a, b) => {
            flatten_ast_and(a, out);
            flatten_ast_and(b, out);
        }
        _ => out.push(printer::formula(f)),
    }
}

fn substitute_ident(f: &ast::Formula, name: &str, replacement: &str) -> ast::Formula {
    let target = ast::Formula::new(FKind::Value(ast::Value::Ident(ast::Name::new(replacement))));
    substitute_ident_with_formula(f, name, &target)
}

fn substitute_ident_with_formula(
    f: &ast::Formula,
    name: &str,
    replacement: &ast::Formula,
) -> ast::Formula {
    let sub = |g: &ast::Formula| substitute_ident_with_formula(g, name, replacement);
    let kind = match &f.kind {
        FKind::Value(ast::Value::Ident(n)) if n.text == name => return replacement.clone(),
        FKind::Value(_) | FKind::Num(_) | FKind::Result => return f.clone(),
        FKind::Call {
            prefix,
            name: n,
            args,
        } => FKind::Call {
            prefix: *prefix,
            name: n.clone(),
            args: args.iter().map(sub).collect(),
        },
        FKind::Forall { ranges, body } => {
            let mut shadowed = false;
            let mut new_ranges = Vec::with_capacity(ranges.len());
            for r in ranges {
                let lo = if shadowed { r.lo.clone() } else { sub(&r.lo) };
                let hi = if shadowed { r.hi.clone() } else { sub(&r.hi) };
                if r.var.text == name {
                    shadowed = true;
                }
                new_ranges.push(ast::Range {
                    var: r.var.clone(),
                    lo,
                    hi,
                });
            }
            let new_body = if shadowed {
                body.as_ref().clone()
            } else {
                sub(body)
            };
            FKind::Forall {
                ranges: new_ranges,
                body: Box::new(new_body),
            }
        }
        FKind::Unary(op, a) => FKind::Unary(*op, Box::new(sub(a))),
        FKind::Binary(op, a, b) => FKind::Binary(*op, Box::new(sub(a)), Box::new(sub(b))),
        FKind::Index(b, idx) => FKind::Index(Box::new(sub(b)), idx.iter().map(sub).collect()),
        FKind::Proj(b, j) => FKind::Proj(Box::new(sub(b)), j.clone()),
    };
    ast::Formula::new(kind)
}

fn bin_prec(b: Bin) -> u8 {
    match b {
        Bin::Implies => 1,
        Bin::Or => 2,
        Bin::And => 3,
        Bin::Eq | Bin::Ne | Bin::Lt | Bin::Le | Bin::Gt | Bin::Ge => 4,
        Bin::Add | Bin::Sub => 5,
        Bin::Mul | Bin::Div | Bin::Mod => 6,
    }
}

fn bin_sym(b: Bin) -> &'static str {
    match b {
        Bin::And => "&&",
        Bin::Or => "||",
        Bin::Implies => "==>",
        Bin::Eq => "==",
        Bin::Ne => "!=",
        Bin::Lt => "<",
        Bin::Le => "<=",
        Bin::Gt => ">",
        Bin::Ge => ">=",
        Bin::Add => "+",
        Bin::Sub => "-",
        Bin::Mul => "*",
        Bin::Div => "/",
        Bin::Mod => "%",
    }
}

fn format_term_inner(
    names: &Names,
    node: &Node,
    term: &Term,
    bound: &[String],
    min_prec: u8,
) -> String {
    let wrap = |s: String, prec: u8| {
        if prec < min_prec { format!("({s})") } else { s }
    };
    match term {
        Term::Input(i) => names.input_name(*i),
        Term::Result(_) if node.results.len() == 1 => "result".to_string(),
        Term::Result(j) => format!("result.{j}"),
        Term::Port(i, s) => names.port_name(*i, *s),
        Term::Bound(d) => bound
            .get(bound.len().wrapping_sub(1 + *d as usize))
            .cloned()
            .unwrap_or_else(|| format!("v{d}")),
        Term::Const(c) => format_const(node, *c),
        Term::Resource(r) => r.name().to_string(),
        Term::Not(a) => {
            let inner = format_term_inner(names, node, a, bound, 7);
            wrap(format!("!{inner}"), 7)
        }
        Term::Neg(a) => {
            let inner = format_term_inner(names, node, a, bound, 7);
            wrap(format!("-{inner}"), 7)
        }
        Term::Bin(b, x, y) => {
            let p = bin_prec(*b);
            let (lp, rp) = match b {
                Bin::Implies => (2, 1),
                _ if p == 4 => (5, 5),
                _ => (p, p + 1),
            };
            let ls = format_term_inner(names, node, x, bound, lp);
            let rs = format_term_inner(names, node, y, bound, rp);
            wrap(format!("{ls} {} {rs}", bin_sym(*b)), p)
        }
        Term::Index(base, idx) => {
            let bs = format_term_inner(names, node, base, bound, 8);
            let is: Vec<String> = idx
                .iter()
                .map(|i| format_term_inner(names, node, i, bound, 0))
                .collect();
            format!("{bs}[{}]", is.join(", "))
        }
        Term::Proj(base, j) => {
            let bs = format_term_inner(names, node, base, bound, 8);
            format!("{bs}.{j}")
        }
        Term::Forall(rs, body) => {
            let mut b = bound.to_vec();
            let mut r_strs = Vec::new();
            for (lo, hi) in rs {
                let ls = format_term_inner(names, node, lo, &b, 5);
                let hs = format_term_inner(names, node, hi, &b, 5);
                let v = format!("v{}", b.len());
                b.push(v.clone());
                r_strs.push(format!("{v} in {ls}..{hs}"));
            }
            let bs = format_term_inner(names, node, body, &b, 0);
            format!("forall({}) {{ {bs} }}", r_strs.join(", "))
        }
        Term::Builtin(b, args) => {
            let as_: Vec<String> = args
                .iter()
                .map(|a| format_term_inner(names, node, a, bound, 0))
                .collect();
            format!("{}({})", b.name(), as_.join(", "))
        }
        Term::Spec(c, args) | Term::Fold(c, args) | Term::Apply(c, args) => {
            let prefix = match term {
                Term::Spec(..) => "spec:",
                Term::Fold(..) => "fold:",
                _ => "",
            };
            let cname = names.callee_name(node, *c);
            let cname = cname.trim_start_matches('@');
            let as_: Vec<String> = args
                .iter()
                .map(|a| format_term_inner(names, node, a, bound, 0))
                .collect();
            format!("{prefix}{cname}({})", as_.join(", "))
        }
    }
}

/// Computes canonical-style Merkle op hashes and invariant hashes for a `Node` (matching `vg_core::canon`).
fn compute_node_op_hashes(n: &Node) -> (Vec<Hash>, Vec<Hash>) {
    let mut op_hash = vec![[0u8; 32]; n.ops.len()];
    let mut inv_hash: Vec<Option<Hash>> = vec![None; n.contract.invariants.len()];

    let input_hash = |i: u32| -> Hash {
        let mut b = vec![0x00];
        b.extend(i.to_le_bytes());
        if let Some(ty) = n.input_type(i) {
            ty.structural_bytes(&mut b);
        }
        derive(Context::Op, &b)
    };
    let result_hash = |op_h: &[Hash], i: u32, s: u8| -> Hash {
        let mut b = vec![0x01];
        b.extend(op_h[i as usize]);
        b.push(s);
        derive(Context::Op, &b)
    };
    let const_bytes = |c: &Const, out: &mut Vec<u8>| {
        c.ty.structural_bytes(out);
        out.extend((c.bytes.len() as u32).to_le_bytes());
        out.extend(&c.bytes);
    };

    fn term_bytes_with_ops(n: &Node, op_h: &[Hash], t: &Term, out: &mut Vec<u8>) {
        let (tag, argc, payload) = term_head(t);
        out.extend(tag.to_le_bytes());
        out.extend(argc.to_le_bytes());
        match t {
            Term::Port(i, s) => {
                let mut b = vec![0x01];
                b.extend(op_h[*i as usize]);
                b.push(*s);
                out.extend(derive(Context::Op, &b));
            }
            Term::Const(c) => {
                let kc = &n.consts[*c as usize];
                kc.ty.structural_bytes(out);
                out.extend((kc.bytes.len() as u32).to_le_bytes());
                out.extend(&kc.bytes);
            }
            Term::Spec(c, _) | Term::Fold(c, _) | Term::Apply(c, _) => {
                out.extend(n.calls[*c as usize]);
            }
            _ => out.extend(payload.to_le_bytes()),
        }
        for c in children(t) {
            term_bytes_with_ops(n, op_h, c, out);
        }
    }

    let get_inv_hash = |v: u32, op_h: &[Hash], cache: &mut [Option<Hash>]| -> Hash {
        if let Some(h) = cache[v as usize] {
            return h;
        }
        let mut b = vec![0x49];
        let conj = &n.contract.invariants[v as usize];
        b.extend((conj.len() as u32).to_le_bytes());
        for t in conj {
            term_bytes_with_ops(n, op_h, t, &mut b);
        }
        let h = derive(Context::Contract, &b);
        cache[v as usize] = Some(h);
        h
    };

    for (i, op) in n.ops.iter().enumerate() {
        let mut op_hashes_for_args: Vec<Hash> = op
            .operands
            .iter()
            .map(|o| match *o {
                Operand::Input(k) => input_hash(k),
                Operand::Op(j, s) => result_hash(&op_hash, j, s),
                Operand::Const(c) => {
                    let mut b = vec![0x02];
                    const_bytes(&n.consts[c as usize], &mut b);
                    derive(Context::Op, &b)
                }
                Operand::Callee(c) => {
                    let mut b = vec![0x03];
                    b.extend(n.calls[c as usize]);
                    derive(Context::Op, &b)
                }
                Operand::Invariant(v) => {
                    let mut b = vec![0x04];
                    b.extend(get_inv_hash(v, &op_hash, &mut inv_hash));
                    derive(Context::Op, &b)
                }
            })
            .collect();
        let k = match opcode::by_code(op.opcode).map(|info| info.comm) {
            Some(Comm::All) => op_hashes_for_args.len(),
            Some(Comm::FirstTwo) => 2.min(op_hashes_for_args.len()),
            _ => 0,
        };
        op_hashes_for_args[..k].sort();
        let mut b = vec![0x05];
        b.extend(op.opcode.to_le_bytes());
        b.push(op.mode);
        b.extend(0u32.to_le_bytes());
        b.push(op.results.len() as u8);
        for t in &op.results {
            t.structural_bytes(&mut b);
        }
        b.extend((op_hashes_for_args.len() as u32).to_le_bytes());
        for h in op_hashes_for_args {
            b.extend(h);
        }
        op_hash[i] = derive(Context::Op, &b);
    }

    let inv_hashes: Vec<Hash> = (0..n.contract.invariants.len() as u32)
        .map(|v| get_inv_hash(v, &op_hash, &mut inv_hash))
        .collect();
    (op_hash, inv_hashes)
}
