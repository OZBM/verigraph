//! Generic SMT solver driver: one long-lived z3, cvc5 or Bitwuzla process over pipes (P3.3).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

use crate::sexpr::{SExpr, read_sexpr};

/// A canonical, quantifier-free SMT-LIB2 query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SmtQuery {
    /// Function/constant declarations mapped to their SMT-LIB2 declaration line suffix,
    /// e.g. `"x"` -> `"(_ BitVec 32)"` for `(declare-const x (_ BitVec 32))`,
    /// or `"fun_foo"` -> `"((_ BitVec 64)) (_ BitVec 8)"` for `(declare-fun ...)`.
    pub decls: BTreeMap<String, String>,
    /// Function declarations: name -> `(arg_sorts, ret_sort)`.
    pub fun_decls: BTreeMap<String, (Vec<String>, String)>,
    /// Assertions in deterministic order.
    pub assertions: Vec<String>,
}

impl SmtQuery {
    pub fn write_body(&self, out: &mut String) {
        for (name, sort) in &self.decls {
            let _ = writeln!(out, "(declare-const {name} {sort})");
        }
        for (name, (args, ret)) in &self.fun_decls {
            let _ = writeln!(out, "(declare-fun {name} ({}) {ret})", args.join(" "));
        }
        for a in &self.assertions {
            let _ = writeln!(out, "(assert {a})");
        }
    }

    pub fn to_smtlib2(&self, timeout_ms: u64) -> String {
        let mut out = String::new();
        out.push_str("(set-option :produce-models true)\n");
        if timeout_ms > 0 {
            let _ = writeln!(out, "(set-option :timeout {timeout_ms})");
        }
        out.push_str("(set-option :smt.random_seed 1)\n");
        out.push_str("(set-option :sat.random_seed 1)\n");
        let mut body = String::new();
        self.write_body(&mut body);
        // Routing (docs/05, P3.3): a query without floating point gets the bitvector logic and a
        // pipeline that substitutes defined constants first, so that equal sub-terms become one
        // (a FOLD.step of SHA-256's round: 119 s with the default tactic, 3.5 s with this one).
        // A tactic changes only speed: UNSAT means the same either way.
        let fp = has_fp(&body);
        if fp {
            out.push_str("(set-logic QF_AUFBVFP)\n");
        } else {
            out.push_str("(set-logic QF_AUFBV)\n");
        }
        out.push_str(&body);
        if fp {
            out.push_str("(check-sat)\n");
        } else {
            out.push_str(
                "(check-sat-using (then simplify propagate-values solve-eqs ctx-simplify simplify smt))\n",
            );
        }
        out
    }

    /// The declarations and assertions, as `write_body` prints them.
    pub fn body(&self) -> String {
        let mut body = String::new();
        self.write_body(&mut body);
        body
    }

    /// The script `kind` runs. cvc5 answers `unsupported` to z3's options and Bitwuzla exits on
    /// them, and both take their time limit on the command line, so they get the bare query.
    pub fn script(&self, kind: SolverKind, timeout_ms: u64) -> String {
        if kind == SolverKind::Z3 {
            return self.to_smtlib2(timeout_ms);
        }
        let body = self.body();
        let logic = if has_fp(&body) {
            "QF_AUFBVFP"
        } else {
            "QF_AUFBV"
        };
        format!("(set-option :produce-models true)\n(set-logic {logic})\n{body}(check-sat)\n")
    }
}

/// Whether SMT-LIB2 text uses floating point.
pub fn has_fp(text: &str) -> bool {
    text.contains("FloatingPoint") || text.contains("(fp.") || text.contains("RNE")
}

/// Whether SMT-LIB2 text uses bitvector division or remainder, where Bitwuzla's bit-blasting and
/// local rewriting are orders of magnitude faster than cvc5's as a second solver.
pub fn has_bv_div(text: &str) -> bool {
    text.contains("(bvsdiv ")
        || text.contains("(bvudiv ")
        || text.contains("(bvsrem ")
        || text.contains("(bvurem ")
}

/// The floating-point formats (eb, sb) that SMT-LIB2 text names. vg-smt writes every format as an
/// indexed identifier, `(_ FloatingPoint eb sb)`, `(_ to_fp eb sb)`, `(_ NaN eb sb)` and so on.
pub fn fp_formats(text: &str) -> BTreeSet<(u32, u32)> {
    let mut out = BTreeSet::new();
    for (i, _) in text.match_indices("(_ ") {
        let mut words = text[i + 3..]
            .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
            .filter(|w| !w.is_empty());
        let (Some(kw), Some(eb), Some(sb)) = (words.next(), words.next(), words.next()) else {
            continue;
        };
        let fp_kw = matches!(
            kw,
            "FloatingPoint"
                | "to_fp"
                | "to_fp_unsigned"
                | "NaN"
                | "+zero"
                | "-zero"
                | "+oo"
                | "-oo"
        );
        if fp_kw && let (Ok(eb), Ok(sb)) = (eb.parse(), sb.parse()) {
            out.insert((eb, sb));
        }
    }
    out
}

/// The solvers of toolchain.lock. Each is deterministic with its default seed (cvc5's is fixed;
/// Bitwuzla's is the constant 27644437), so only z3 is given seed options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SolverKind {
    Z3,
    Cvc5,
    Bitwuzla,
}

impl SolverKind {
    pub fn name(self) -> &'static str {
        match self {
            SolverKind::Z3 => "z3",
            SolverKind::Cvc5 => "cvc5",
            SolverKind::Bitwuzla => "bitwuzla",
        }
    }

    fn args(self, timeout_ms: u64) -> Vec<String> {
        let mut a: Vec<String> = match self {
            SolverKind::Z3 => vec!["-in".into()],
            SolverKind::Cvc5 => vec!["--lang=smt2".into(), "--incremental".into()],
            SolverKind::Bitwuzla => vec!["--lang".into(), "smt2".into()],
        };
        if timeout_ms > 0 {
            match self {
                SolverKind::Z3 => {}
                SolverKind::Cvc5 => a.push(format!("--tlimit-per={timeout_ms}")),
                SolverKind::Bitwuzla => a.extend(["-T".into(), timeout_ms.to_string()]),
            }
        }
        a
    }

    /// Whether this solver takes every floating-point format of `text` without experimental
    /// options. Probed on the pinned versions: cvc5 1.4.2 takes only Float32 and Float64,
    /// Bitwuzla 0.11.0 adds Float16 and Float128, both reject BFloat16 (8, 8); z3 takes all.
    /// A literal `(fp …)` names its format only by bit widths, so it is left to z3.
    pub fn supports(self, text: &str) -> bool {
        let ok: &[(u32, u32)] = match self {
            SolverKind::Z3 => return true,
            SolverKind::Cvc5 => &[(8, 24), (11, 53)],
            SolverKind::Bitwuzla => &[(5, 11), (8, 24), (11, 53), (15, 113)],
        };
        !text.contains("(fp ") && fp_formats(text).iter().all(|f| ok.contains(f))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckResult {
    Unsat,
    Sat,
    Unknown { timeout: bool, reason: String },
    Error(String),
}

pub trait Solver {
    fn name(&self) -> &str;
    fn check_query(&mut self, query: &SmtQuery) -> CheckResult;
    fn get_values(&mut self, exprs: &[String]) -> Result<Vec<SExpr>, String>;
}

/// One long-lived solver process over stdin/stdout pipes, reset before every query.
pub struct Session {
    kind: SolverKind,
    path: PathBuf,
    timeout_ms: u64,
    /// z3's resource limit per query; 0 for none.
    rlimit: u64,
    proc: Option<LiveProc>,
}

/// The z3 session, under the name its first callers use.
pub type Z3Session = Session;

struct LiveProc {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    /// A z3 session.
    pub fn new(z3_path: &Path, timeout_ms: u64) -> Self {
        Self::of(SolverKind::Z3, z3_path, timeout_ms)
    }

    pub fn of(kind: SolverKind, path: &Path, timeout_ms: u64) -> Self {
        Self {
            kind,
            path: path.to_path_buf(),
            timeout_ms,
            rlimit: 0,
            proc: None,
        }
    }

    /// Bounds each query by z3's resource counter, which, unlike the clock, stops a query at the
    /// same point on every machine, so a search that gives up gives up everywhere. z3 only.
    pub fn with_rlimit(mut self, rlimit: u64) -> Self {
        assert_eq!(self.kind, SolverKind::Z3, "rlimit is a z3 option");
        self.rlimit = rlimit;
        self
    }

    fn ensure_started(&mut self) -> Result<&mut LiveProc, String> {
        if self.proc.is_none() {
            let name = self.kind.name();
            let mut child = Command::new(&self.path)
                .args(self.kind.args(self.timeout_ms))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("failed to start {name} at {}: {e}", self.path.display()))?;
            let stdin = BufWriter::new(child.stdin.take().ok_or("missing solver stdin")?);
            let stdout = BufReader::new(child.stdout.take().ok_or("missing solver stdout")?);
            self.proc = Some(LiveProc {
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
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(LiveProc {
            mut child,
            mut stdin,
            ..
        }) = self.proc.take()
        {
            let _ = stdin.write_all(b"(exit)\n");
            let _ = stdin.flush();
            // Closing stdin ends a solver that ignores `(exit)`, so the wait cannot hang.
            drop(stdin);
            let _ = child.wait();
        }
    }
}

impl Solver for Session {
    fn name(&self) -> &str {
        self.kind.name()
    }

    fn check_query(&mut self, query: &SmtQuery) -> CheckResult {
        let name = self.kind.name();
        // `(reset)`, not push and pop, which disables qffp in z3 5.4.1.
        let rlimit = if self.rlimit > 0 {
            format!("(set-option :rlimit {})\n", self.rlimit)
        } else {
            String::new()
        };
        let script = format!(
            "(reset)\n{rlimit}{}",
            query.script(self.kind, self.timeout_ms)
        );
        let start = Instant::now();

        let res = {
            let proc = match self.ensure_started() {
                Ok(p) => p,
                Err(e) => return CheckResult::Error(e),
            };
            if let Err(e) = proc
                .stdin
                .write_all(script.as_bytes())
                .and_then(|()| proc.stdin.flush())
            {
                Err(format!("writing query to {name} failed: {e}"))
            } else {
                match read_sexpr(&mut proc.stdout) {
                    Ok(Some(expr)) => Ok(expr),
                    Ok(None) => Err(format!("unexpected EOF from {name}")),
                    Err(e) => Err(format!("reading {name} response failed: {e}")),
                }
            }
        };

        let expr = match res {
            Ok(e) => e,
            Err(msg) => {
                self.kill_proc();
                return CheckResult::Error(msg);
            }
        };

        match expr {
            SExpr::Atom(ref a) if a == "unsat" => CheckResult::Unsat,
            SExpr::Atom(ref a) if a == "sat" => CheckResult::Sat,
            // Bitwuzla has no :reason-unknown and exits on the command; it decides these logics,
            // so its unknown is the time limit, which the clock confirms.
            SExpr::Atom(ref a) if a == "unknown" && self.kind == SolverKind::Bitwuzla => {
                let timeout = self.timeout_ms > 0
                    && start.elapsed().as_millis() >= u128::from(self.timeout_ms);
                let reason = if timeout { "timeout" } else { "unknown" };
                CheckResult::Unknown {
                    timeout,
                    reason: reason.into(),
                }
            }
            SExpr::Atom(ref a) if a == "unknown" => {
                let reason = match self.read_reason_unknown() {
                    Ok(r) => r,
                    Err(e) => {
                        self.kill_proc();
                        return CheckResult::Error(e);
                    }
                };
                let lower = reason.to_ascii_lowercase();
                let timeout = lower.contains("timeout") || lower.contains("canceled");
                CheckResult::Unknown { timeout, reason }
            }
            SExpr::List(ref xs) if xs.first().and_then(SExpr::as_atom) == Some("error") => {
                let msg = xs
                    .get(1)
                    .and_then(SExpr::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| expr.to_string());
                self.kill_proc();
                CheckResult::Error(format!("{name} error: {msg}"))
            }
            other => {
                self.kill_proc();
                CheckResult::Error(format!("unexpected {name} response: {other}"))
            }
        }
    }

    fn get_values(&mut self, exprs: &[String]) -> Result<Vec<SExpr>, String> {
        if exprs.is_empty() {
            return Ok(Vec::new());
        }
        let mut cmd = String::from("(get-value (");
        for (i, e) in exprs.iter().enumerate() {
            if i > 0 {
                cmd.push(' ');
            }
            cmd.push_str(e);
        }
        cmd.push_str("))\n");

        let resp = {
            let proc = self.proc.as_mut().ok_or("solver session is not active")?;
            match proc
                .stdin
                .write_all(cmd.as_bytes())
                .and_then(|()| proc.stdin.flush())
            {
                Err(e) => Err(format!("writing get-value failed: {e}")),
                Ok(()) => match read_sexpr(&mut proc.stdout) {
                    Ok(Some(e)) => Ok(e),
                    Ok(None) => Err("unexpected EOF reading get-value".to_string()),
                    Err(e) => Err(format!("reading get-value failed: {e}")),
                },
            }
        };
        // A solver that failed here (Bitwuzla exits on an error) must not serve the next query.
        let resp = resp.inspect_err(|_| self.kill_proc())?;

        let SExpr::List(pairs) = resp else {
            self.kill_proc();
            return Err(format!("expected list from get-value, got {resp}"));
        };
        if pairs.first().and_then(SExpr::as_atom) == Some("error") {
            self.kill_proc();
            return Err(format!("{} get-value error: {pairs:?}", self.kind.name()));
        }
        if pairs.len() != exprs.len() {
            return Err(format!(
                "get-value returned {} items for {} queries",
                pairs.len(),
                exprs.len()
            ));
        }
        let mut out = Vec::with_capacity(pairs.len());
        for pair in pairs {
            let SExpr::List(items) = pair else {
                return Err(format!("malformed get-value pair: {pair}"));
            };
            if items.len() != 2 {
                return Err(format!("malformed get-value pair length: {items:?}"));
            }
            out.push(items.into_iter().nth(1).expect("len 2"));
        }
        Ok(out)
    }
}

impl Session {
    fn read_reason_unknown(&mut self) -> Result<String, String> {
        let proc = self.proc.as_mut().ok_or("solver session not active")?;
        proc.stdin
            .write_all(b"(get-info :reason-unknown)\n")
            .and_then(|()| proc.stdin.flush())
            .map_err(|e| format!("writing get-info failed: {e}"))?;
        let resp = match read_sexpr(&mut proc.stdout) {
            Ok(Some(e)) => e,
            Ok(None) => return Err("unexpected EOF reading :reason-unknown".to_string()),
            Err(e) => return Err(format!("reading :reason-unknown failed: {e}")),
        };
        if let SExpr::List(items) = &resp
            && items.len() == 2
        {
            if let Some(s) = items[1].as_str() {
                return Ok(s.to_string());
            }
            if let Some(a) = items[1].as_atom() {
                return Ok(a.to_string());
            }
        }
        Ok(resp.to_string())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::{parse_bv_u128, parse_float};
    use vg_core::types::Type;

    /// `name`'s path; a missing solver skips the test, unless VG_REQUIRE_SOLVERS=1 (as the
    /// release-mode suites need) turns that into a failure.
    pub(crate) fn installed(name: &str) -> Option<PathBuf> {
        let p = crate::find_solver(name);
        if p.is_none() {
            assert!(
                std::env::var("VG_REQUIRE_SOLVERS").as_deref() != Ok("1"),
                "{name} is not installed and VG_REQUIRE_SOLVERS=1"
            );
            eprintln!("skipped: {name} is not installed");
        }
        p
    }

    /// One session answers two queries in a row, the second after a reset, and its model decodes.
    fn smoke(kind: SolverKind, path: &Path) {
        let mut sess = Session::of(kind, path, 2000);

        let mut q1 = SmtQuery::default();
        q1.decls.insert("x".into(), "(_ BitVec 32)".into());
        q1.assertions
            .push("(not (= (bvadd x (_ bv1 32)) (bvadd (_ bv1 32) x)))".into());
        assert_eq!(sess.check_query(&q1), CheckResult::Unsat, "{kind:?}");

        let mut q2 = SmtQuery::default();
        q2.decls.insert("x".into(), "(_ BitVec 32)".into());
        q2.decls.insert("y".into(), "(_ FloatingPoint 8 24)".into());
        q2.assertions.push("(= x #x0000002a)".into());
        q2.assertions.push("(= y (_ -zero 8 24))".into());
        assert_eq!(sess.check_query(&q2), CheckResult::Sat, "{kind:?}");
        let vals = sess.get_values(&["x".into(), "y".into()]).unwrap();
        assert_eq!(parse_bv_u128(&vals[0]), Ok(42), "{kind:?}");
        assert_eq!(
            parse_float(&vals[1], &Type::Float(32)),
            Ok(0x8000_0000),
            "{kind:?}"
        );
    }

    #[test]
    fn z3_smoke_reset_and_models() {
        let z3 = crate::find_z3();
        smoke(SolverKind::Z3, &z3);
        // z3 prints these values in its own forms.
        let mut sess = Z3Session::new(&z3, 2000);
        let mut q = SmtQuery::default();
        q.decls.insert("y".into(), "(_ FloatingPoint 8 24)".into());
        q.assertions.push("(= y (_ -zero 8 24))".into());
        assert_eq!(sess.check_query(&q), CheckResult::Sat);
        let vals = sess.get_values(&["y".into()]).unwrap();
        assert_eq!(vals[0].to_string(), "(_ -zero 8 24)");
    }

    /// A z3 query over its resource limit is unknown long before the clock runs out, and the next
    /// one is answered. (Under the bitvector tactic z3 calls either limit "canceled".)
    #[test]
    fn z3_rlimit_stops_a_query_and_goes_on() {
        let mut hard = SmtQuery::default();
        hard.decls.insert("a".into(), "(_ BitVec 64)".into());
        hard.decls.insert("b".into(), "(_ BitVec 64)".into());
        hard.assertions
            .push("(= (bvmul a b) #x00000c3c0c3c0c3d)".into());
        hard.assertions.push("(bvult #x0000000000000001 a)".into());
        hard.assertions.push("(bvult #x0000000000000001 b)".into());
        let mut sess = Z3Session::new(&crate::find_z3(), 60_000).with_rlimit(1000);
        let start = Instant::now();
        assert!(matches!(
            sess.check_query(&hard),
            CheckResult::Unknown { .. }
        ));
        assert!(start.elapsed().as_secs() < 10);
        let mut easy = SmtQuery::default();
        easy.decls.insert("z".into(), "(_ BitVec 8)".into());
        easy.assertions.push("(= z #x01)".into());
        assert_eq!(sess.check_query(&easy), CheckResult::Sat);
    }

    /// A solver that dies during get-value is not reused: the next query starts a new process.
    #[cfg(unix)]
    #[test]
    fn a_solver_that_dies_in_get_value_is_restarted() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("vg-fake-dies-{}", std::process::id()));
        let script = "#!/bin/sh\nwhile IFS= read -r l; do case \"$l\" in\n\
                      *'(check-sat)'*) echo sat;;\n*get-value*) exit 1;;\nesac; done\n";
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut sess = Session::of(SolverKind::Cvc5, &path, 2000);
        let mut q = SmtQuery::default();
        q.decls.insert("x".into(), "(_ BitVec 8)".into());
        assert_eq!(sess.check_query(&q), CheckResult::Sat);
        assert!(sess.get_values(&["x".into()]).is_err());
        assert_eq!(sess.check_query(&q), CheckResult::Sat);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cvc5_and_bitwuzla_smoke_reset_and_models() {
        for kind in [SolverKind::Cvc5, SolverKind::Bitwuzla] {
            if let Some(path) = installed(kind.name()) {
                smoke(kind, &path);
            }
        }
    }

    /// A query past the time limit is unknown with `timeout`, and the next one is answered.
    #[test]
    fn cvc5_and_bitwuzla_time_out_and_go_on() {
        // Factoring a 64-bit semiprime with two 32-bit factors: far beyond 300 ms.
        let mut hard = SmtQuery::default();
        hard.decls.insert("a".into(), "(_ BitVec 64)".into());
        hard.decls.insert("b".into(), "(_ BitVec 64)".into());
        for a in [
            "(= (bvmul a b) #x00000c3c0c3c0c3d)",
            "(not (= a #x0000000000000001))",
            "(not (= b #x0000000000000001))",
            "(bvult a #x00000000ffffffff)",
            "(bvult b #x00000000ffffffff)",
        ] {
            hard.assertions.push(a.into());
        }
        let mut easy = SmtQuery::default();
        easy.decls.insert("z".into(), "(_ BitVec 8)".into());
        easy.assertions
            .push("(not (= (bvadd z #x01) (bvadd #x01 z)))".into());
        for kind in [SolverKind::Cvc5, SolverKind::Bitwuzla] {
            let Some(path) = installed(kind.name()) else {
                continue;
            };
            let mut sess = Session::of(kind, &path, 300);
            match sess.check_query(&hard) {
                CheckResult::Unknown { timeout, .. } => assert!(timeout, "{kind:?}"),
                other => panic!("{kind:?}: {other:?}"),
            }
            assert_eq!(sess.check_query(&easy), CheckResult::Unsat, "{kind:?}");
        }
    }

    #[test]
    fn float_formats_route_to_the_solvers_that_take_them() {
        let bv = "(declare-const x (_ BitVec 32))\n(assert (= x (_ bv1 32)))";
        let f32 = "(declare-const y (_ FloatingPoint 8 24))\n(assert (= y (_ NaN 8 24)))";
        let f16 = "(assert (fp.lt ((_ to_fp 5 11) RNE x) (_ +zero 5 11)))";
        let bf16 = "(declare-const y (_ FloatingPoint 8 8))";
        let literal = "(assert (fp.isNaN (fp #b0 #b11111111 #b0000001)))";
        assert!(fp_formats(bv).is_empty());
        assert_eq!(fp_formats(f16), BTreeSet::from([(5, 11)]));
        assert_eq!(
            fp_formats(&format!("{f32}{bf16}")),
            BTreeSet::from([(8, 8), (8, 24)])
        );
        use SolverKind::{Bitwuzla, Cvc5, Z3};
        for (text, want) in [
            (bv, [true, true, true]),
            (f32, [true, true, true]),
            (f16, [true, false, true]),
            (bf16, [true, false, false]),
            (literal, [true, false, false]),
        ] {
            assert_eq!(
                [Z3, Cvc5, Bitwuzla].map(|k| k.supports(text)),
                want,
                "{text}"
            );
        }
    }

    /// cvc5 and Bitwuzla get no z3 option: cvc5 answers `unsupported` and Bitwuzla exits.
    #[test]
    fn only_z3_scripts_carry_z3_options() {
        let mut q = SmtQuery::default();
        q.decls.insert("x".into(), "(_ BitVec 8)".into());
        q.assertions.push("(= x #x01)".into());
        assert!(q.script(SolverKind::Z3, 500).contains(":timeout 500"));
        for kind in [SolverKind::Cvc5, SolverKind::Bitwuzla] {
            let s = q.script(kind, 500);
            assert_eq!(
                s,
                "(set-option :produce-models true)\n(set-logic QF_AUFBV)\n\
                 (declare-const x (_ BitVec 8))\n(assert (= x #x01))\n(check-sat)\n"
            );
        }
    }
}
