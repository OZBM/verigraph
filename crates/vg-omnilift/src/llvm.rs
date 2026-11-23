//! P8.2: Parser for the normalized LLVM IR textual subset.

use std::collections::{BTreeMap, BTreeSet};

use vg_core::diag::{Diagnostic, Span};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LlvmType {
    Void,
    Int(u32),
    Ptr,
    Array(u64, Box<LlvmType>),
    Struct { packed: bool, fields: Vec<LlvmType> },
    Named(String),
}

impl LlvmType {
    pub fn bit_width(&self) -> Option<u32> {
        match self {
            LlvmType::Int(w) => Some(*w),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobalInit {
    IntArray { elem_bits: u32, values: Vec<i128> },
    ByteString(Vec<u8>),
    ZeroInit,
    Opaque,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Global {
    pub name: String,
    pub is_constant: bool,
    pub ty: LlvmType,
    pub init: GlobalInit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub ty: LlvmType,
    pub name: String,
    pub unnamed: bool,
    pub dereferenceable: Option<u64>,
    pub readonly: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueRef {
    Local(String),
    Global(String),
    ConstInt { bits: u32, value: i128 },
    ConstBool(bool),
    ConstNull,
    Undef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOpKind {
    Add,
    Sub,
    Mul,
    UDiv,
    SDiv,
    URem,
    SRem,
    And,
    Or,
    Xor,
    Shl,
    LShr,
    AShr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastKind {
    ZExt,
    SExt,
    Trunc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ICmpPred {
    Eq,
    Ne,
    UGt,
    UGe,
    ULt,
    ULe,
    SGt,
    SGe,
    SLt,
    SLe,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstKind {
    Phi {
        ty: LlvmType,
        incoming: Vec<(ValueRef, String)>,
    },
    BinOp {
        op: BinOpKind,
        nuw: bool,
        nsw: bool,
        exact: bool,
        disjoint: bool,
        ty: LlvmType,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    Cast {
        op: CastKind,
        nneg: bool,
        nuw: bool,
        nsw: bool,
        from_ty: LlvmType,
        val: ValueRef,
        to_ty: LlvmType,
    },
    ICmp {
        samesign: bool,
        pred: ICmpPred,
        ty: LlvmType,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    Select {
        cond: ValueRef,
        ty: LlvmType,
        then_val: ValueRef,
        else_val: ValueRef,
    },
    Alloca {
        ty: LlvmType,
        count: Option<(u32, ValueRef)>,
        align: Option<u32>,
    },
    Gep {
        inbounds: bool,
        nuw: bool,
        base_ty: LlvmType,
        ptr: ValueRef,
        indices: Vec<(LlvmType, ValueRef)>,
    },
    Load {
        ty: LlvmType,
        ptr: ValueRef,
        align: Option<u32>,
    },
    Store {
        ty: LlvmType,
        val: ValueRef,
        ptr: ValueRef,
        align: Option<u32>,
    },
    Call {
        tail: bool,
        ret_ty: LlvmType,
        callee: String,
        args: Vec<(LlvmType, ValueRef)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inst {
    pub result: Option<String>,
    pub kind: InstKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminator {
    Br {
        target: String,
    },
    CondBr {
        cond: ValueRef,
        then_bb: String,
        else_bb: String,
    },
    Ret(Option<(LlvmType, ValueRef)>),
    Unreachable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasicBlock {
    pub label: String,
    pub insts: Vec<Inst>,
    pub term: Terminator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub ret_ty: LlvmType,
    pub params: Vec<Param>,
    pub blocks: Vec<BasicBlock>,
}

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub types: BTreeMap<String, LlvmType>,
    pub globals: BTreeMap<String, Global>,
    pub functions: BTreeMap<String, Function>,
    raw_bodies: BTreeMap<String, RawFn>,
}

#[derive(Clone, Debug)]
struct RawFn {
    name: String,
    ret_ty: LlvmType,
    params: Vec<Param>,
    body_lines: Vec<String>,
}

fn err(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error("VG-L0001", Span::default(), msg)
}

impl Module {
    /// Byte size of `ty` under aarch64-apple-darwin data layout.
    pub fn sizeof(&self, ty: &LlvmType) -> Result<u64, Diagnostic> {
        Ok(self.size_align(ty)?.0)
    }

    pub fn alignof(&self, ty: &LlvmType) -> Result<u64, Diagnostic> {
        Ok(self.size_align(ty)?.1)
    }

    pub fn size_align(&self, ty: &LlvmType) -> Result<(u64, u64), Diagnostic> {
        match ty {
            LlvmType::Void => Ok((0, 1)),
            LlvmType::Int(bits) => {
                let bytes = u64::from((*bits).max(8).div_ceil(8));
                let align = match *bits {
                    0..=8 => 1,
                    9..=16 => 2,
                    17..=32 => 4,
                    _ => 8,
                };
                Ok((bytes, align))
            }
            LlvmType::Ptr => Ok((8, 8)),
            LlvmType::Array(n, elem) => {
                let (esz, eal) = self.size_align(elem)?;
                let stride = align_to(esz, eal);
                Ok((n * stride, eal))
            }
            LlvmType::Struct { packed, fields } => {
                let mut offset = 0u64;
                let mut max_align = 1u64;
                for f in fields {
                    let (fsz, fal) = self.size_align(f)?;
                    let eff_al = if *packed { 1 } else { fal };
                    max_align = max_align.max(eff_al);
                    offset = align_to(offset, eff_al) + fsz;
                }
                Ok((align_to(offset, max_align), max_align))
            }
            LlvmType::Named(name) => {
                let resolved = self
                    .types
                    .get(name)
                    .ok_or_else(|| err(format!("unknown LLVM struct type %{name}")))?;
                self.size_align(resolved)
            }
        }
    }

    /// Byte offset and field type for a struct GEP index.
    pub fn struct_field(
        &self,
        ty: &LlvmType,
        field_idx: usize,
    ) -> Result<(u64, LlvmType), Diagnostic> {
        let resolved = match ty {
            LlvmType::Named(name) => self
                .types
                .get(name)
                .ok_or_else(|| err(format!("unknown LLVM struct type %{name}")))?,
            other => other,
        };
        let LlvmType::Struct { packed, fields } = resolved else {
            return Err(err(format!("struct GEP into non-struct type {ty:?}")));
        };
        if field_idx >= fields.len() {
            return Err(err(format!(
                "struct GEP field index {field_idx} out of bounds for {} fields",
                fields.len()
            )));
        }
        let mut offset = 0u64;
        for (idx, f) in fields.iter().enumerate() {
            let (fsz, fal) = self.size_align(f)?;
            let eff_al = if *packed { 1 } else { fal };
            offset = align_to(offset, eff_al);
            if idx == field_idx {
                return Ok((offset, f.clone()));
            }
            offset += fsz;
        }
        unreachable!()
    }
}

fn align_to(val: u64, align: u64) -> u64 {
    if align <= 1 {
        val
    } else {
        (val + align - 1) & !(align - 1)
    }
}

/// Parse EVERY function in `ir` (used in parser unit tests and small modules).
pub fn parse_module(ir: &str) -> Result<Module, Diagnostic> {
    let mut m = scan_module(ir)?;
    let names: Vec<String> = m.raw_bodies.keys().cloned().collect();
    for name in names {
        ensure_function_parsed(&mut m, &name)?;
    }
    Ok(m)
}

/// Parse `entry` and all internal functions transitively called by `entry`.
pub fn parse_module_reachable(ir: &str, entry: &str) -> Result<Module, Diagnostic> {
    let mut m = scan_module(ir)?;
    if !m.raw_bodies.contains_key(entry) {
        return Err(err(format!(
            "function @{entry} not found in normalized LLVM IR"
        )));
    }
    let mut queue = vec![entry.to_string()];
    let mut seen = BTreeSet::new();
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        ensure_function_parsed(&mut m, &name)?;
        if let Some(func) = m.functions.get(&name) {
            for bb in &func.blocks {
                for inst in &bb.insts {
                    if let InstKind::Call { callee, .. } = &inst.kind
                        && m.raw_bodies.contains_key(callee)
                        && !seen.contains(callee)
                    {
                        queue.push(callee.clone());
                    }
                }
            }
        }
    }
    Ok(m)
}

fn ensure_function_parsed(m: &mut Module, name: &str) -> Result<(), Diagnostic> {
    if m.functions.contains_key(name) {
        return Ok(());
    }
    let raw = m
        .raw_bodies
        .get(name)
        .ok_or_else(|| err(format!("function @{name} not found in LLVM IR")))?
        .clone();
    let func = parse_function_body(&raw)?;
    m.functions.insert(name.to_string(), func);
    Ok(())
}

fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    let mut escaped = false;
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_str {
            if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else if b == b'"' {
            in_str = true;
        } else if b == b';' {
            return &line[..i];
        }
    }
    line
}

fn scan_module(ir: &str) -> Result<Module, Diagnostic> {
    let mut m = Module::default();
    let lines: Vec<&str> = ir.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = strip_comment(lines[i]).trim();
        i += 1;
        if line.is_empty()
            || line.starts_with("source_filename")
            || line.starts_with("target ")
            || line.starts_with("attributes ")
            || line.starts_with("declare ")
            || line.starts_with('!')
        {
            continue;
        }

        if line.starts_with('%') && line.contains("= type ") {
            let (name, rhs) = line
                .split_once("= type ")
                .ok_or_else(|| err(format!("malformed type definition: {line}")))?;
            let name = unquote_ident(name.trim().trim_start_matches('%'));
            let (ty, _) = parse_type(rhs.trim())?;
            m.types.insert(name, ty);
            continue;
        }

        if line.starts_with('@') {
            let g = parse_global(line)?;
            m.globals.insert(g.name.clone(), g);
            continue;
        }

        if line.starts_with("define ") {
            let mut header = line.to_string();
            while !header.ends_with('{') && i < lines.len() {
                let next = strip_comment(lines[i]).trim();
                i += 1;
                header.push(' ');
                header.push_str(next);
            }
            if !header.ends_with('{') {
                return Err(err(format!("unterminated function header: {header}")));
            }
            let (name, ret_ty, params) = parse_define_header(&header)?;
            let mut body_lines = Vec::new();
            let mut closed = false;
            while i < lines.len() {
                let raw_line = lines[i];
                i += 1;
                let trimmed = strip_comment(raw_line).trim();
                if trimmed == "}" {
                    closed = true;
                    break;
                }
                if !trimmed.is_empty() {
                    body_lines.push(trimmed.to_string());
                }
            }
            if !closed {
                return Err(err(format!("unterminated function @{name}")));
            }
            m.raw_bodies.insert(
                name.clone(),
                RawFn {
                    name,
                    ret_ty,
                    params,
                    body_lines,
                },
            );
            continue;
        }

        return Err(err(format!("unsupported top-level LLVM IR line: {line}")));
    }
    Ok(m)
}

fn unquote_ident(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn parse_global(line: &str) -> Result<Global, Diagnostic> {
    let (lhs, rhs) = line
        .split_once('=')
        .ok_or_else(|| err(format!("malformed global: {line}")))?;
    let name = unquote_ident(lhs.trim().trim_start_matches('@'));
    let mut rest = rhs.trim();

    // Skip linkage / visibility / unnamed_addr / addrspace / dso_local keywords.
    let is_constant;
    loop {
        if let Some(after) = strip_keyword(rest, "constant") {
            is_constant = true;
            rest = after;
            break;
        } else if let Some(after) = strip_keyword(rest, "global") {
            is_constant = false;
            rest = after;
            break;
        } else if let Some(after) = skip_one_attr(rest) {
            rest = after;
        } else {
            return Err(err(format!("malformed global @{name}: {line}")));
        }
    }

    let (ty, after_ty) = parse_type(rest)?;
    let init_part = strip_trailing_align(after_ty.trim());
    let init = parse_global_init(&ty, init_part);
    Ok(Global {
        name,
        is_constant,
        ty,
        init,
    })
}

fn strip_trailing_align(s: &str) -> &str {
    // Strip trailing `, align N` or `, !...` outside brackets.
    let bytes = s.as_bytes();
    let mut depth_sq = 0i32;
    let mut depth_br = 0i32;
    let mut depth_pa = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (idx, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_str {
            if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' => depth_sq += 1,
            b']' => depth_sq -= 1,
            b'{' => depth_br += 1,
            b'}' => depth_br -= 1,
            b'(' => depth_pa += 1,
            b')' => depth_pa -= 1,
            b',' if depth_sq == 0 && depth_br == 0 && depth_pa == 0 => {
                let tail = s[idx + 1..].trim();
                if tail.starts_with("align ") || tail.starts_with('!') || tail.starts_with('#') {
                    return s[..idx].trim();
                }
            }
            _ => {}
        }
    }
    s.trim()
}

fn parse_global_init(ty: &LlvmType, s: &str) -> GlobalInit {
    let s = s.trim();
    if s == "zeroinitializer" {
        return GlobalInit::ZeroInit;
    }
    if s.starts_with("c\"") && s.ends_with('"') {
        return GlobalInit::ByteString(parse_c_string(&s[2..s.len() - 1]));
    }
    if let LlvmType::Array(_, elem) = ty
        && let LlvmType::Int(bits) = **elem
        && s.starts_with('[')
        && s.ends_with(']')
    {
        let inner = &s[1..s.len() - 1];
        let mut values = Vec::new();
        for item in split_top_level(inner, ',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let Some((_, val_str)) = item.split_once(char::is_whitespace) else {
                return GlobalInit::Opaque;
            };
            let Ok(v) = val_str.trim().parse::<i128>() else {
                return GlobalInit::Opaque;
            };
            values.push(v);
        }
        return GlobalInit::IntArray {
            elem_bits: bits,
            values,
        };
    }
    GlobalInit::Opaque
}

fn parse_c_string(raw: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 2 < bytes.len()
            && let Ok(hex_str) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(val) = u8::from_str_radix(hex_str, 16)
        {
            out.push(val);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn strip_keyword<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix(kw)
        && (rest.is_empty()
            || rest.starts_with(|c: char| c.is_whitespace() || c == '(' || c == ','))
    {
        return Some(rest.trim_start());
    }
    None
}

fn skip_balanced_parens(s: &str) -> Option<&str> {
    let s = s.trim_start();
    if !s.starts_with('(') {
        return Some(s);
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (idx, b) in s.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_str {
            if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[idx + 1..]);
                }
            }
            _ => {}
        }
    }
    None
}

fn skip_one_attr(s: &str) -> Option<&str> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    const ATTR_WORDS: &[&str] = &[
        "private",
        "internal",
        "available_externally",
        "linkonce",
        "weak",
        "common",
        "appending",
        "extern_weak",
        "linkonce_odr",
        "weak_odr",
        "external",
        "dso_local",
        "dso_preemptable",
        "local_unnamed_addr",
        "unnamed_addr",
        "hidden",
        "protected",
        "default",
        "fastcc",
        "ccc",
        "zeroext",
        "signext",
        "inreg",
        "byval",
        "byref",
        "preallocated",
        "inalloca",
        "sret",
        "noalias",
        "nocapture",
        "captures",
        "initializes",
        "nofree",
        "nest",
        "returned",
        "nonnull",
        "dereferenceable",
        "dereferenceable_or_null",
        "swiftself",
        "swifterror",
        "immarg",
        "noundef",
        "readonly",
        "writeonly",
        "readnone",
        "writable",
        "dead_on_unwind",
        "range",
        "align",
        "allocalign",
        "allocptr",
        "addrspace",
        "externally_initialized",
        "thread_local",
        "cold",
        "noreturn",
    ];
    for &kw in ATTR_WORDS {
        if let Some(after) = strip_keyword(s, kw) {
            if kw == "align" || kw == "addrspace" {
                let rest = if after.starts_with('(') {
                    skip_balanced_parens(after)?.trim_start()
                } else {
                    let end = after
                        .find(|c: char| c.is_whitespace() || c == ',' || c == ')')
                        .unwrap_or(after.len());
                    after[end..].trim_start()
                };
                return Some(rest);
            }
            if after.starts_with('(') {
                return skip_balanced_parens(after).map(str::trim_start);
            }
            return Some(after);
        }
    }
    None
}

fn parse_type(s: &str) -> Result<(LlvmType, &str), Diagnostic> {
    let s = s.trim_start();
    if let Some(rest) = strip_keyword(s, "void") {
        return Ok((LlvmType::Void, rest));
    }
    if let Some(rest) = strip_keyword(s, "ptr") {
        return Ok((LlvmType::Ptr, rest));
    }
    if let Some(rest) = s.strip_prefix('i') {
        let digits_len = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits_len > 0 {
            let bits: u32 = rest[..digits_len]
                .parse()
                .map_err(|_| err(format!("invalid integer type in {s}")))?;
            let after = &rest[digits_len..];
            if after.is_empty()
                || after.starts_with(|c: char| {
                    c.is_whitespace() || matches!(c, ',' | ')' | ']' | '}' | '>')
                })
            {
                return Ok((LlvmType::Int(bits), after.trim_start()));
            }
        }
    }
    if s.starts_with('[') {
        let mut depth = 0i32;
        let mut end = None;
        for (idx, b) in s.bytes().enumerate() {
            match b {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(idx);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.ok_or_else(|| err(format!("unterminated array type: {s}")))?;
        let inner = &s[1..end];
        let (count_str, elem_str) = inner
            .split_once('x')
            .ok_or_else(|| err(format!("malformed array type: {s}")))?;
        let count: u64 = count_str
            .trim()
            .parse()
            .map_err(|_| err(format!("invalid array length in {s}")))?;
        let (elem_ty, tail) = parse_type(elem_str)?;
        if !tail.trim().is_empty() {
            return Err(err(format!(
                "unexpected trailing tokens in array type: {s}"
            )));
        }
        return Ok((
            LlvmType::Array(count, Box::new(elem_ty)),
            s[end + 1..].trim_start(),
        ));
    }
    if s.starts_with("<{") || s.starts_with('{') {
        let packed = s.starts_with("<{");
        let open_len = if packed { 2 } else { 1 };
        let mut depth = 0i32;
        let mut end = None;
        let bytes = s.as_bytes();
        let mut idx = 0;
        while idx < bytes.len() {
            match bytes[idx] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(idx);
                        break;
                    }
                }
                _ => {}
            }
            idx += 1;
        }
        let end = end.ok_or_else(|| err(format!("unterminated struct type: {s}")))?;
        let inner = &s[open_len..end];
        let mut fields = Vec::new();
        for part in split_top_level(inner, ',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (fty, tail) = parse_type(part)?;
            if !tail.trim().is_empty() {
                return Err(err(format!("malformed struct field type: {part}")));
            }
            fields.push(fty);
        }
        let close_len = if packed {
            if !s[end..].starts_with("}>") {
                return Err(err(format!("expected }}> in packed struct: {s}")));
            }
            2
        } else {
            1
        };
        return Ok((
            LlvmType::Struct { packed, fields },
            s[end + close_len..].trim_start(),
        ));
    }
    if let Some(rest) = s.strip_prefix('%') {
        let (tok, after) = take_ident_or_quoted(rest)?;
        return Ok((LlvmType::Named(tok), after.trim_start()));
    }
    Err(err(format!("unsupported LLVM type: {s}")))
}

fn take_ident_or_quoted(s: &str) -> Result<(String, &str), Diagnostic> {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        let mut escaped = false;
        for (idx, b) in rest.bytes().enumerate() {
            if escaped {
                escaped = false;
                continue;
            }
            if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                return Ok((rest[..idx].to_string(), &rest[idx + 1..]));
            }
        }
        return Err(err(format!("unterminated quoted identifier: {s}")));
    }
    let len = s
        .bytes()
        .take_while(|&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'$' | b'-'))
        .count();
    if len == 0 {
        return Err(err(format!("expected identifier in `{s}`")));
    }
    Ok((s[..len].to_string(), &s[len..]))
}

fn parse_define_header(header: &str) -> Result<(String, LlvmType, Vec<Param>), Diagnostic> {
    let mut rest =
        strip_keyword(header, "define").ok_or_else(|| err(format!("expected define: {header}")))?;
    while let Some(next) = skip_one_attr(rest) {
        rest = next;
    }
    let (ret_ty, after_ret) = parse_type(rest)?;
    let at_pos = after_ret
        .find('@')
        .ok_or_else(|| err(format!("expected function @name in `{header}`")))?;
    let (name, after_name) = take_ident_or_quoted(&after_ret[at_pos + 1..])?;
    let after_name = after_name.trim_start();
    if !after_name.starts_with('(') {
        return Err(err(format!("expected parameter list for @{name}")));
    }
    let after_parens = skip_balanced_parens(after_name)
        .ok_or_else(|| err(format!("unterminated parameter list for @{name}")))?;
    let params_len = after_name.len() - after_parens.len();
    let params_inner = &after_name[1..params_len - 1];

    let mut params = Vec::new();
    for raw_p in split_top_level(params_inner, ',') {
        let raw_p = raw_p.trim();
        if raw_p.is_empty() {
            continue;
        }
        if raw_p == "..." {
            return Err(err(format!("variadic function @{name} not supported")));
        }
        params.push(parse_param(raw_p)?);
    }
    Ok((name, ret_ty, params))
}

fn parse_param(raw: &str) -> Result<Param, Diagnostic> {
    let (ty, mut rest) = parse_type(raw)?;
    let mut dereferenceable = None;
    let mut readonly = false;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            return Err(err(format!("parameter without name in `{raw}`")));
        }
        if let Some(after_pct) = trimmed.strip_prefix('%') {
            let (raw_name, tail) = take_ident_or_quoted(after_pct)?;
            if !tail.trim().is_empty() {
                return Err(err(format!("unexpected trailing tokens in param `{raw}`")));
            }
            let unnamed = raw_name.bytes().all(|b| b.is_ascii_digit());
            return Ok(Param {
                ty,
                name: raw_name,
                unnamed,
                dereferenceable,
                readonly,
            });
        }
        if let Some(after) = strip_keyword(trimmed, "readonly") {
            readonly = true;
            rest = after;
            continue;
        }
        if let Some(after) = strip_keyword(trimmed, "dereferenceable")
            && let Some(inner_start) = after.strip_prefix('(')
            && let Some(end) = inner_start.find(')')
        {
            if let Ok(n) = inner_start[..end].trim().parse::<u64>() {
                dereferenceable = Some(n);
            }
            rest = inner_start[end + 1..].trim_start();
            continue;
        }
        if let Some(after) = skip_one_attr(trimmed) {
            rest = after;
        } else {
            return Err(err(format!("unsupported parameter attribute in `{raw}`")));
        }
    }
}

fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth_pa = 0i32;
    let mut depth_sq = 0i32;
    let mut depth_br = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut start = 0usize;
    for (idx, ch) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_str {
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '(' => depth_pa += 1,
            ')' => depth_pa -= 1,
            '[' => depth_sq += 1,
            ']' => depth_sq -= 1,
            '{' => depth_br += 1,
            '}' => depth_br -= 1,
            c if c == sep && depth_pa == 0 && depth_sq == 0 && depth_br == 0 => {
                out.push(&s[start..idx]);
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn is_block_label_line(line: &str) -> Option<&str> {
    // A label line is `<label>:` at top level (not inside quotes).
    if line.starts_with('%')
        || line.starts_with("ret ")
        || line == "ret void"
        || line == "unreachable"
    {
        return None;
    }
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_str {
            if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        if b == b'"' {
            in_str = true;
            continue;
        }
        if b == b':' && i + 1 == bytes.len() {
            return Some(&line[..i]);
        }
        if b.is_ascii_whitespace() || b == b'=' {
            return None;
        }
    }
    None
}

fn parse_function_body(raw: &RawFn) -> Result<Function, Diagnostic> {
    let unnamed_count = raw.params.iter().filter(|p| p.unnamed).count();
    let mut blocks = Vec::new();
    let mut cur_label: Option<String> = None;
    let mut cur_insts: Vec<Inst> = Vec::new();

    for line in &raw.body_lines {
        if let Some(lbl) = is_block_label_line(line) {
            if cur_label.is_some() || !cur_insts.is_empty() {
                return Err(err(format!(
                    "basic block without terminator before `{lbl}:` in @{}",
                    raw.name
                )));
            }
            cur_label = Some(unquote_ident(lbl));
            continue;
        }

        if cur_label.is_none() {
            let entry_lbl = if blocks.is_empty() {
                unnamed_count.to_string()
            } else {
                return Err(err(format!("instruction outside basic block: {line}")));
            };
            cur_label = Some(entry_lbl);
        }

        let cleaned = strip_trailing_inst_metadata(line);
        if let Some(term) = try_parse_terminator(cleaned)? {
            let label = cur_label.take().unwrap();
            blocks.push(BasicBlock {
                label,
                insts: std::mem::take(&mut cur_insts),
                term,
            });
        } else {
            cur_insts.push(parse_instruction(cleaned)?);
        }
    }

    if cur_label.is_some() || !cur_insts.is_empty() {
        return Err(err(format!(
            "unterminated basic block at end of @{}",
            raw.name
        )));
    }
    if blocks.is_empty() {
        return Err(err(format!("function @{} has no basic blocks", raw.name)));
    }

    Ok(Function {
        name: raw.name.clone(),
        ret_ty: raw.ret_ty.clone(),
        params: raw.params.clone(),
        blocks,
    })
}

fn strip_trailing_inst_metadata(s: &str) -> &str {
    // Strip `, !meta !10` or trailing `#7` outside quotes/brackets/parens.
    let mut out_end = s.len();
    let bytes = s.as_bytes();
    let mut depth_pa = 0i32;
    let mut depth_sq = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (idx, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_str {
            if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'(' => depth_pa += 1,
            b')' => depth_pa -= 1,
            b'[' => depth_sq += 1,
            b']' => depth_sq -= 1,
            b',' if depth_pa == 0 && depth_sq == 0 => {
                let tail = s[idx + 1..].trim_start();
                if tail.starts_with('!') {
                    out_end = idx;
                    break;
                }
            }
            _ => {}
        }
    }
    let trimmed = s[..out_end].trim();
    // Also strip trailing attribute group ` #7` if preceded by `)`.
    if let Some(hash_pos) = trimmed.rfind(" #") {
        let after = &trimmed[hash_pos + 2..];
        if !after.is_empty() && after.bytes().all(|b| b.is_ascii_digit()) {
            return trimmed[..hash_pos].trim_end();
        }
    }
    trimmed
}

fn try_parse_terminator(line: &str) -> Result<Option<Terminator>, Diagnostic> {
    if line == "unreachable" {
        return Ok(Some(Terminator::Unreachable));
    }
    if line == "ret void" {
        return Ok(Some(Terminator::Ret(None)));
    }
    if let Some(rest) = strip_keyword(line, "ret") {
        let (ty, after_ty) = parse_type(rest)?;
        let val = parse_value_ref(after_ty.trim(), &ty)?;
        return Ok(Some(Terminator::Ret(Some((ty, val)))));
    }
    if let Some(rest) = strip_keyword(line, "br") {
        let parts = split_top_level(rest, ',');
        if parts.len() == 1 {
            let p = parts[0].trim();
            let lbl_part = strip_keyword(p, "label")
                .ok_or_else(|| err(format!("malformed unconditional br: {line}")))?;
            let lbl = parse_label_ref(lbl_part)?;
            return Ok(Some(Terminator::Br { target: lbl }));
        }
        if parts.len() == 3 {
            let (cond_ty, cond_rest) = parse_type(parts[0].trim())?;
            if cond_ty != LlvmType::Int(1) {
                return Err(err(format!("br condition must be i1 in `{line}`")));
            }
            let cond = parse_value_ref(cond_rest.trim(), &cond_ty)?;
            let then_part = strip_keyword(parts[1].trim(), "label")
                .ok_or_else(|| err(format!("malformed br then label: {line}")))?;
            let else_part = strip_keyword(parts[2].trim(), "label")
                .ok_or_else(|| err(format!("malformed br else label: {line}")))?;
            return Ok(Some(Terminator::CondBr {
                cond,
                then_bb: parse_label_ref(then_part)?,
                else_bb: parse_label_ref(else_part)?,
            }));
        }
        return Err(err(format!("malformed br instruction: {line}")));
    }
    if line.starts_with("switch ") || line.starts_with("indirectbr ") || line.starts_with("invoke ")
    {
        let op = line.split_whitespace().next().unwrap_or(line);
        return Err(err(format!("unsupported LLVM IR construct: `{op}`")));
    }
    Ok(None)
}

fn parse_label_ref(s: &str) -> Result<String, Diagnostic> {
    let s = s.trim();
    let rest = s
        .strip_prefix('%')
        .ok_or_else(|| err(format!("expected %label, found `{s}`")))?;
    let (name, tail) = take_ident_or_quoted(rest)?;
    if !tail.trim().is_empty() {
        return Err(err(format!("unexpected trailing tokens after label `{s}`")));
    }
    Ok(name)
}

fn parse_value_ref(s: &str, ty: &LlvmType) -> Result<ValueRef, Diagnostic> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('%') {
        let (name, tail) = take_ident_or_quoted(rest)?;
        if !tail.trim().is_empty() {
            return Err(err(format!("unexpected trailing tokens after value `{s}`")));
        }
        return Ok(ValueRef::Local(name));
    }
    if let Some(rest) = s.strip_prefix('@') {
        let (name, tail) = take_ident_or_quoted(rest)?;
        if !tail.trim().is_empty() {
            return Err(err(format!(
                "unexpected trailing tokens after global `{s}`"
            )));
        }
        return Ok(ValueRef::Global(name));
    }
    if s == "true" {
        return Ok(ValueRef::ConstBool(true));
    }
    if s == "false" {
        return Ok(ValueRef::ConstBool(false));
    }
    if s == "null" {
        return Ok(ValueRef::ConstNull);
    }
    if s == "undef" || s == "poison" {
        return Ok(ValueRef::Undef);
    }
    if let Ok(v) = s.parse::<i128>() {
        let bits = ty.bit_width().unwrap_or(64);
        if bits == 1 {
            return Ok(ValueRef::ConstBool(v != 0));
        }
        return Ok(ValueRef::ConstInt { bits, value: v });
    }
    Err(err(format!("unsupported LLVM operand value `{s}`")))
}

fn parse_instruction(line: &str) -> Result<Inst, Diagnostic> {
    // Check assignment `<%res> = <rhs>` carefully without splitting inside quotes/brackets.
    let (result, rhs) = if line.starts_with('%') {
        let eq = line
            .find('=')
            .ok_or_else(|| err(format!("malformed SSA instruction: {line}")))?;
        let lhs = line[..eq].trim();
        let (name, tail) = take_ident_or_quoted(lhs.trim_start_matches('%'))?;
        if !tail.trim().is_empty() {
            return Err(err(format!("malformed SSA result `{lhs}`")));
        }
        (Some(name), line[eq + 1..].trim())
    } else {
        (None, line)
    };

    let (tail_call, rhs_no_tail) = if let Some(after) = strip_keyword(rhs, "tail") {
        (true, after)
    } else if let Some(after) = strip_keyword(rhs, "musttail") {
        (true, after)
    } else if let Some(after) = strip_keyword(rhs, "notail") {
        (false, after)
    } else {
        (false, rhs)
    };

    let opcode = rhs_no_tail
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap_or("");

    let kind = match opcode {
        "phi" => parse_phi(strip_keyword(rhs_no_tail, "phi").unwrap())?,
        "add" | "sub" | "mul" | "udiv" | "sdiv" | "urem" | "srem" | "and" | "or" | "xor"
        | "shl" | "lshr" | "ashr" => {
            let bin_op = match opcode {
                "add" => BinOpKind::Add,
                "sub" => BinOpKind::Sub,
                "mul" => BinOpKind::Mul,
                "udiv" => BinOpKind::UDiv,
                "sdiv" => BinOpKind::SDiv,
                "urem" => BinOpKind::URem,
                "srem" => BinOpKind::SRem,
                "and" => BinOpKind::And,
                "or" => BinOpKind::Or,
                "xor" => BinOpKind::Xor,
                "shl" => BinOpKind::Shl,
                "lshr" => BinOpKind::LShr,
                "ashr" => BinOpKind::AShr,
                _ => unreachable!(),
            };
            parse_binop(bin_op, strip_keyword(rhs_no_tail, opcode).unwrap())?
        }
        "zext" | "sext" | "trunc" => {
            let cast_op = match opcode {
                "zext" => CastKind::ZExt,
                "sext" => CastKind::SExt,
                "trunc" => CastKind::Trunc,
                _ => unreachable!(),
            };
            parse_cast(cast_op, strip_keyword(rhs_no_tail, opcode).unwrap())?
        }
        "icmp" => parse_icmp(strip_keyword(rhs_no_tail, "icmp").unwrap())?,
        "select" => parse_select(strip_keyword(rhs_no_tail, "select").unwrap())?,
        "alloca" => parse_alloca(strip_keyword(rhs_no_tail, "alloca").unwrap())?,
        "getelementptr" => parse_gep(strip_keyword(rhs_no_tail, "getelementptr").unwrap())?,
        "load" => parse_load(strip_keyword(rhs_no_tail, "load").unwrap())?,
        "store" => parse_store(strip_keyword(rhs_no_tail, "store").unwrap())?,
        "call" => parse_call(tail_call, strip_keyword(rhs_no_tail, "call").unwrap())?,
        other => {
            return Err(err(format!(
                "unsupported LLVM IR construct: `{other}` in `{line}`"
            )));
        }
    };

    Ok(Inst { result, kind })
}

fn parse_phi(rest: &str) -> Result<InstKind, Diagnostic> {
    let (ty, after_ty) = parse_type(rest)?;
    let mut incoming = Vec::new();
    for item in split_top_level(after_ty.trim(), ',') {
        let item = item.trim();
        if !item.starts_with('[') || !item.ends_with(']') {
            return Err(err(format!("malformed phi pair `{item}`")));
        }
        let inner = &item[1..item.len() - 1];
        let parts = split_top_level(inner, ',');
        if parts.len() != 2 {
            return Err(err(format!("malformed phi pair `{item}`")));
        }
        let val = parse_value_ref(parts[0].trim(), &ty)?;
        let bb = parse_label_ref(parts[1].trim())?;
        incoming.push((val, bb));
    }
    Ok(InstKind::Phi { ty, incoming })
}

fn parse_binop(op: BinOpKind, mut rest: &str) -> Result<InstKind, Diagnostic> {
    let (mut nuw, mut nsw, mut exact, mut disjoint) = (false, false, false, false);
    loop {
        if let Some(after) = strip_keyword(rest, "nuw") {
            nuw = true;
            rest = after;
        } else if let Some(after) = strip_keyword(rest, "nsw") {
            nsw = true;
            rest = after;
        } else if let Some(after) = strip_keyword(rest, "exact") {
            exact = true;
            rest = after;
        } else if let Some(after) = strip_keyword(rest, "disjoint") {
            disjoint = true;
            rest = after;
        } else {
            break;
        }
    }
    let (ty, after_ty) = parse_type(rest)?;
    let parts = split_top_level(after_ty, ',');
    if parts.len() != 2 {
        return Err(err(format!("malformed binary op operands `{after_ty}`")));
    }
    let lhs = parse_value_ref(parts[0].trim(), &ty)?;
    let rhs = parse_value_ref(parts[1].trim(), &ty)?;
    Ok(InstKind::BinOp {
        op,
        nuw,
        nsw,
        exact,
        disjoint,
        ty,
        lhs,
        rhs,
    })
}

fn parse_cast(op: CastKind, mut rest: &str) -> Result<InstKind, Diagnostic> {
    let (mut nneg, mut nuw, mut nsw) = (false, false, false);
    loop {
        if let Some(after) = strip_keyword(rest, "nneg") {
            nneg = true;
            rest = after;
        } else if let Some(after) = strip_keyword(rest, "nuw") {
            nuw = true;
            rest = after;
        } else if let Some(after) = strip_keyword(rest, "nsw") {
            nsw = true;
            rest = after;
        } else {
            break;
        }
    }
    let (from_ty, after_from) = parse_type(rest)?;
    let to_pos = after_from
        .rfind(" to ")
        .ok_or_else(|| err(format!("expected `to` in cast `{rest}`")))?;
    let val = parse_value_ref(after_from[..to_pos].trim(), &from_ty)?;
    let (to_ty, tail) = parse_type(after_from[to_pos + 4..].trim())?;
    if !tail.trim().is_empty() {
        return Err(err(format!("trailing tokens in cast `{rest}`")));
    }
    Ok(InstKind::Cast {
        op,
        nneg,
        nuw,
        nsw,
        from_ty,
        val,
        to_ty,
    })
}

fn parse_icmp(mut rest: &str) -> Result<InstKind, Diagnostic> {
    let mut samesign = false;
    if let Some(after) = strip_keyword(rest, "samesign") {
        samesign = true;
        rest = after;
    }
    let pred_tok = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| err("missing icmp predicate"))?;
    let pred = match pred_tok {
        "eq" => ICmpPred::Eq,
        "ne" => ICmpPred::Ne,
        "ugt" => ICmpPred::UGt,
        "uge" => ICmpPred::UGe,
        "ult" => ICmpPred::ULt,
        "ule" => ICmpPred::ULe,
        "sgt" => ICmpPred::SGt,
        "sge" => ICmpPred::SGe,
        "slt" => ICmpPred::SLt,
        "sle" => ICmpPred::SLe,
        other => return Err(err(format!("unsupported icmp predicate `{other}`"))),
    };
    let after_pred = strip_keyword(rest, pred_tok).unwrap();
    let (ty, after_ty) = parse_type(after_pred)?;
    let parts = split_top_level(after_ty, ',');
    if parts.len() != 2 {
        return Err(err(format!("malformed icmp operands `{after_ty}`")));
    }
    let lhs = parse_value_ref(parts[0].trim(), &ty)?;
    let rhs = parse_value_ref(parts[1].trim(), &ty)?;
    Ok(InstKind::ICmp {
        samesign,
        pred,
        ty,
        lhs,
        rhs,
    })
}

fn parse_select(rest: &str) -> Result<InstKind, Diagnostic> {
    let parts = split_top_level(rest, ',');
    if parts.len() != 3 {
        return Err(err(format!("malformed select `{rest}`")));
    }
    let (cond_ty, cond_rest) = parse_type(parts[0].trim())?;
    if cond_ty != LlvmType::Int(1) {
        return Err(err(format!("select condition must be i1 in `{rest}`")));
    }
    let cond = parse_value_ref(cond_rest.trim(), &cond_ty)?;
    let (ty, then_rest) = parse_type(parts[1].trim())?;
    let then_val = parse_value_ref(then_rest.trim(), &ty)?;
    let (else_ty, else_rest) = parse_type(parts[2].trim())?;
    if else_ty != ty {
        return Err(err(format!("select arm types differ in `{rest}`")));
    }
    let else_val = parse_value_ref(else_rest.trim(), &ty)?;
    Ok(InstKind::Select {
        cond,
        ty,
        then_val,
        else_val,
    })
}

fn parse_alloca(rest: &str) -> Result<InstKind, Diagnostic> {
    let parts = split_top_level(rest, ',');
    let (ty, tail) = parse_type(parts[0].trim())?;
    if !tail.trim().is_empty() {
        return Err(err(format!("unexpected tokens in alloca `{rest}`")));
    }
    let mut count = None;
    let mut align = None;
    for extra in &parts[1..] {
        let extra = extra.trim();
        if let Some(after) = strip_keyword(extra, "align") {
            let a = after
                .trim()
                .parse::<u32>()
                .map_err(|_| err(format!("invalid alloca align `{extra}`")))?;
            align = Some(a);
        } else {
            let (cty, crest) = parse_type(extra)?;
            let bits = cty
                .bit_width()
                .ok_or_else(|| err(format!("invalid alloca count `{extra}`")))?;
            let cval = parse_value_ref(crest.trim(), &cty)?;
            count = Some((bits, cval));
        }
    }
    Ok(InstKind::Alloca { ty, count, align })
}

fn parse_gep(mut rest: &str) -> Result<InstKind, Diagnostic> {
    let mut inbounds = false;
    let mut nuw = false;
    loop {
        if let Some(after) = strip_keyword(rest, "inbounds") {
            inbounds = true;
            rest = after;
        } else if let Some(after) = strip_keyword(rest, "nuw") {
            nuw = true;
            rest = after;
        } else if let Some(after) = strip_keyword(rest, "nusw") {
            rest = after;
        } else {
            break;
        }
    }
    let parts = split_top_level(rest, ',');
    if parts.len() < 3 {
        return Err(err(format!("malformed getelementptr `{rest}`")));
    }
    let (base_ty, tail) = parse_type(parts[0].trim())?;
    if !tail.trim().is_empty() {
        return Err(err(format!("malformed getelementptr base type `{rest}`")));
    }
    let (ptr_ty, ptr_rest) = parse_type(parts[1].trim())?;
    if ptr_ty != LlvmType::Ptr {
        return Err(err(format!("getelementptr base must be ptr in `{rest}`")));
    }
    let ptr = parse_value_ref(ptr_rest.trim(), &LlvmType::Ptr)?;
    let mut indices = Vec::with_capacity(parts.len() - 2);
    for idx_part in &parts[2..] {
        let (ity, irest) = parse_type(idx_part.trim())?;
        let ival = parse_value_ref(irest.trim(), &ity)?;
        indices.push((ity, ival));
    }
    Ok(InstKind::Gep {
        inbounds,
        nuw,
        base_ty,
        ptr,
        indices,
    })
}

fn parse_load(mut rest: &str) -> Result<InstKind, Diagnostic> {
    if let Some(after) = strip_keyword(rest, "volatile") {
        return Err(err(format!(
            "unsupported LLVM IR construct: volatile load `{after}`"
        )));
    }
    let parts = split_top_level(rest, ',');
    if parts.len() < 2 {
        return Err(err(format!("malformed load `{rest}`")));
    }
    let (ty, tail) = parse_type(parts[0].trim())?;
    if !tail.trim().is_empty() {
        return Err(err(format!("malformed load type `{rest}`")));
    }
    let (ptr_ty, ptr_rest) = parse_type(parts[1].trim())?;
    if ptr_ty != LlvmType::Ptr {
        return Err(err(format!("load operand must be ptr in `{rest}`")));
    }
    let ptr = parse_value_ref(ptr_rest.trim(), &LlvmType::Ptr)?;
    let mut align = None;
    for extra in &parts[2..] {
        let extra = extra.trim();
        if let Some(after) = strip_keyword(extra, "align") {
            align = Some(
                after
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| err(format!("invalid load align `{extra}`")))?,
            );
        } else {
            rest = extra;
            return Err(err(format!("unsupported load modifier `{rest}`")));
        }
    }
    Ok(InstKind::Load { ty, ptr, align })
}

fn parse_store(rest: &str) -> Result<InstKind, Diagnostic> {
    if strip_keyword(rest, "volatile").is_some() {
        return Err(err("unsupported LLVM IR construct: volatile store"));
    }
    let parts = split_top_level(rest, ',');
    if parts.len() < 2 {
        return Err(err(format!("malformed store `{rest}`")));
    }
    let (ty, val_rest) = parse_type(parts[0].trim())?;
    let val = parse_value_ref(val_rest.trim(), &ty)?;
    let (ptr_ty, ptr_rest) = parse_type(parts[1].trim())?;
    if ptr_ty != LlvmType::Ptr {
        return Err(err(format!("store target must be ptr in `{rest}`")));
    }
    let ptr = parse_value_ref(ptr_rest.trim(), &LlvmType::Ptr)?;
    let mut align = None;
    for extra in &parts[2..] {
        let extra = extra.trim();
        if let Some(after) = strip_keyword(extra, "align") {
            align = Some(
                after
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| err(format!("invalid store align `{extra}`")))?,
            );
        } else {
            return Err(err(format!("unsupported store modifier `{extra}`")));
        }
    }
    Ok(InstKind::Store {
        ty,
        val,
        ptr,
        align,
    })
}

fn parse_call(tail: bool, mut rest: &str) -> Result<InstKind, Diagnostic> {
    while let Some(after) = skip_one_attr(rest) {
        rest = after;
    }
    let (ret_ty, after_ret) = parse_type(rest)?;
    let after_ret = after_ret.trim_start();
    if after_ret.starts_with('%') {
        return Err(err("unsupported LLVM IR construct: indirect call"));
    }
    let after_at = after_ret
        .strip_prefix('@')
        .ok_or_else(|| err(format!("expected @callee in call `{rest}`")))?;
    let (callee, after_name) = take_ident_or_quoted(after_at)?;
    let after_name = after_name.trim_start();
    if !after_name.starts_with('(') {
        return Err(err(format!("expected argument list in call @{callee}")));
    }
    let after_parens = skip_balanced_parens(after_name)
        .ok_or_else(|| err(format!("unterminated argument list in call @{callee}")))?;
    let args_len = after_name.len() - after_parens.len();
    let args_inner = &after_name[1..args_len - 1];

    if let Some(intrinsic_rest) = callee.strip_prefix("llvm.") {
        const ALLOWED_PREFIXES: &[&str] = &[
            "fshl.",
            "fshr.",
            "bswap.",
            "memcpy.",
            "memset.",
            "lifetime.start.",
            "lifetime.end.",
            "assume",
            "experimental.noalias.scope.decl",
            "umin.",
            "umax.",
            "smin.",
            "smax.",
        ];
        if !ALLOWED_PREFIXES
            .iter()
            .any(|p| intrinsic_rest == *p || intrinsic_rest.starts_with(p))
        {
            return Err(err(format!(
                "unsupported LLVM IR construct: intrinsic @{callee}"
            )));
        }
    }

    let mut args = Vec::new();
    for raw_arg in split_top_level(args_inner, ',') {
        let raw_arg = raw_arg.trim();
        if raw_arg.is_empty() {
            continue;
        }
        if raw_arg.starts_with("metadata ") {
            continue;
        }
        let (aty, mut arest) = parse_type(raw_arg)?;
        while let Some(after) = skip_one_attr(arest) {
            arest = after;
        }
        let aval = parse_value_ref(arest.trim(), &aty)?;
        args.push((aty, aval));
    }

    Ok(InstKind::Call {
        tail,
        ret_ty,
        callee,
        args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arithmetic_flags_phi_icmp_select_and_loops() {
        let ir = r#"
; comment
source_filename = "demo.c"
target triple = "arm64-apple-macosx27.2.0"

@tbl = internal constant [4 x i32] [i32 0, i32 -1, i32 42, i32 100], align 4

define range(i64 0, 4294967296) i64 @sum_masked(ptr nofree noundef readonly captures(address_is_null) %0, i64 noundef %1) local_unnamed_addr #0 {
  %3 = icmp eq ptr %0, null
  br i1 %3, label %15, label %4

4:                                                ; preds = %2
  %5 = icmp eq i64 %1, 0
  br i1 %5, label %15, label %.preheader

.preheader:
  br label %6

6:                                                ; preds = %.preheader, %6
  %7 = phi i64 [ 0, %.preheader ], [ %13, %6 ]
  %8 = phi i64 [ 0, %.preheader ], [ %12, %6 ]
  %9 = getelementptr inbounds nuw i8, ptr %0, i64 %7
  %10 = load i8, ptr %9, align 1, !tbaa !10
  %11 = zext nneg i8 %10 to i64
  %12 = add nuw nsw i64 %8, %11
  %13 = add nuw nsw i64 %7, 1
  %14 = icmp samesign ult i64 %13, %1
  br i1 %14, label %6, label %15, !llvm.loop !12

15:
  %16 = phi i64 [ 0, %2 ], [ 0, %4 ], [ %12, %6 ]
  ret i64 %16
}
"#;
        let m = parse_module(ir).unwrap();
        assert_eq!(m.globals.len(), 1);
        assert!(matches!(
            &m.globals["tbl"].init,
            GlobalInit::IntArray { elem_bits: 32, values } if values == &[0, -1, 42, 100]
        ));
        let f = &m.functions["sum_masked"];
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.blocks[0].label, "2"); // 2 unnamed params -> implicit entry %2
        assert_eq!(f.blocks.len(), 5);
    }

    #[test]
    fn parses_alloca_struct_gep_store_and_intrinsics() {
        let ir = r#"
%struct.anon = type { i32, i32, [64 x i32], [8 x i32] }

define void @f(ptr noalias noundef align 4 captures(none) dereferenceable(32) %state) {
start:
  %tmp = alloca %struct.anon, align 4
  call void @llvm.lifetime.start.p0(ptr nonnull %tmp)
  %p = getelementptr inbounds nuw i8, ptr %tmp, i64 264
  call void @llvm.memcpy.p0.p0.i64(ptr noundef nonnull align 4 dereferenceable(32) %p, ptr noundef nonnull align 4 dereferenceable(32) %state, i64 32, i1 false)
  %v = load i32, ptr %p, align 4
  %r = tail call i32 @llvm.fshl.i32(i32 %v, i32 %v, i32 26)
  %b = tail call noundef i32 @llvm.bswap.i32(i32 %r)
  store i32 %b, ptr %state, align 4
  call void @llvm.lifetime.end.p0(ptr nonnull %tmp)
  ret void
}
"#;
        let m = parse_module(ir).unwrap();
        assert_eq!(
            m.sizeof(&LlvmType::Named("struct.anon".into())).unwrap(),
            296
        );
        assert_eq!(m.functions["f"].blocks[0].label, "start");
    }

    #[test]
    fn rejects_unsupported_llvm_constructs_with_vg_l0001() {
        let bad_cases = [
            "define float @f(float %a, float %b) {\n  %r = fadd float %a, %b\n  ret float %r\n}\n",
            "define i32 @f(i32 %a) {\n  switch i32 %a, label %d [ i32 0, label %d ]\nd:\n  ret i32 0\n}\n",
            "define i64 @f(ptr %p) {\n  %r = ptrtoint ptr %p to i64\n  ret i64 %r\n}\n",
            "define i32 @f(ptr %fp) {\n  %r = call i32 %fp()\n  ret i32 %r\n}\n",
            "define i32 @f(i32 %x) {\n  %r = call i32 @llvm.ctpop.i32(i32 %x)\n  ret i32 %r\n}\n",
        ];
        for ir in bad_cases {
            let d = parse_module(ir).unwrap_err();
            assert_eq!(d.code, "VG-L0001", "expected VG-L0001 for:\n{ir}");
        }
    }

    #[test]
    fn lazy_reachable_parsing_skips_unrelated_rust_helpers() {
        let ir = r#"
define i64 @dead_rust_helper(ptr %a, ptr %b) {
  %x = ptrtoint ptr %a to i64
  %y = ptrtoint ptr %b to i64
  %z = sub i64 %x, %y
  ret i64 %z
}

define i32 @target_fn(i32 %a, i32 %b) {
  %r = add i32 %a, %b
  ret i32 %r
}
"#;
        assert_eq!(parse_module(ir).unwrap_err().code, "VG-L0001");
        let m = parse_module_reachable(ir, "target_fn").unwrap();
        assert!(m.functions.contains_key("target_fn"));
        assert!(!m.functions.contains_key("dead_rust_helper"));
    }

    #[test]
    fn ingests_and_parses_all_four_vendored_targets() {
        if std::env::var("VG_REQUIRE_ALIVE2").as_deref() != Ok("1") && !crate::ingest::has_opt() {
            eprintln!("SKIP: opt not installed on this host");
            return;
        }
        use crate::ingest::{Language, ingest_to_llvm_ir};
        use std::path::Path;

        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let cases = [
            (root.join("tests/lift/zlib/crc32.c"), "crc32_z", Language::C),
            (
                root.join("tests/lift/zlib/adler32.c"),
                "adler32_z",
                Language::C,
            ),
            (
                root.join("tests/lift/mbedtls/sha256_shim.c"),
                "mbedtls_sha256_compress",
                Language::C,
            ),
            (
                root.join("tests/lift/rust-sha2/lib.rs"),
                "compress256",
                Language::Rust,
            ),
        ];
        for (path, func, lang) in cases {
            let ing = ingest_to_llvm_ir(&path, func, lang, &[]).unwrap();
            let m = parse_module_reachable(&ing.llvm_ir, func).unwrap();
            assert!(
                m.functions.contains_key(func),
                "missing parsed function {func}"
            );
        }
    }
}
