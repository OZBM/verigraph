//! Generic SMT solver driver and long-lived `z3 -in` pipe session.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

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
        out.push_str("(set-logic QF_AUFBVFP)\n");
        self.write_body(&mut out);
        out.push_str("(check-sat)\n");
        out
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

/// One long-lived `z3 -in` process over stdin/stdout pipes.
pub struct Z3Session {
    z3_path: PathBuf,
    timeout_ms: u64,
    proc: Option<LiveProc>,
}

struct LiveProc {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Z3Session {
    pub fn new(z3_path: &Path, timeout_ms: u64) -> Self {
        Self {
            z3_path: z3_path.to_path_buf(),
            timeout_ms,
            proc: None,
        }
    }

    fn ensure_started(&mut self) -> Result<&mut LiveProc, String> {
        if self.proc.is_none() {
            let mut child = Command::new(&self.z3_path)
                .arg("-in")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("failed to start z3 at {}: {e}", self.z3_path.display()))?;
            let stdin = BufWriter::new(child.stdin.take().ok_or("missing z3 stdin")?);
            let stdout = BufReader::new(child.stdout.take().ok_or("missing z3 stdout")?);
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

impl Drop for Z3Session {
    fn drop(&mut self) {
        if let Some(mut p) = self.proc.take() {
            let _ = p.stdin.write_all(b"(exit)\n");
            let _ = p.stdin.flush();
            let _ = p.child.wait();
        }
    }
}

impl Solver for Z3Session {
    fn name(&self) -> &str {
        "z3"
    }

    fn check_query(&mut self, query: &SmtQuery) -> CheckResult {
        let timeout_ms = self.timeout_ms;
        // Build the full command buffer with `(reset)` (NOT push/pop, which disables qffp in Z3 5.4.1).
        let mut script = String::with_capacity(4096);
        script.push_str("(reset)\n");
        script.push_str(&query.to_smtlib2(timeout_ms));

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
                Err(format!("writing query to z3 failed: {e}"))
            } else {
                match read_sexpr(&mut proc.stdout) {
                    Ok(Some(expr)) => Ok(expr),
                    Ok(None) => Err("unexpected EOF from z3".to_string()),
                    Err(e) => Err(format!("reading z3 response failed: {e}")),
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
                CheckResult::Error(format!("z3 error: {msg}"))
            }
            other => {
                self.kill_proc();
                CheckResult::Error(format!("unexpected z3 response: {other}"))
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
            let proc = self.proc.as_mut().ok_or("z3 session is not active")?;
            proc.stdin
                .write_all(cmd.as_bytes())
                .and_then(|()| proc.stdin.flush())
                .map_err(|e| format!("writing get-value failed: {e}"))?;
            match read_sexpr(&mut proc.stdout) {
                Ok(Some(e)) => e,
                Ok(None) => return Err("unexpected EOF reading get-value".to_string()),
                Err(e) => return Err(format!("reading get-value failed: {e}")),
            }
        };

        let SExpr::List(pairs) = resp else {
            return Err(format!("expected list from get-value, got {resp}"));
        };
        if pairs.first().and_then(SExpr::as_atom) == Some("error") {
            self.kill_proc();
            return Err(format!("z3 get-value error: {pairs:?}"));
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

impl Z3Session {
    fn read_reason_unknown(&mut self) -> Result<String, String> {
        let proc = self.proc.as_mut().ok_or("z3 session not active")?;
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
mod tests {
    use super::*;

    #[test]
    fn z3_smoke_reset_and_models() {
        let z3 = crate::find_z3();
        let mut sess = Z3Session::new(&z3, 2000);

        let mut q1 = SmtQuery::default();
        q1.decls.insert("x".into(), "(_ BitVec 32)".into());
        q1.assertions
            .push("(not (= (bvadd x (_ bv1 32)) (bvadd (_ bv1 32) x)))".into());
        assert_eq!(sess.check_query(&q1), CheckResult::Unsat);

        let mut q2 = SmtQuery::default();
        q2.decls.insert("x".into(), "(_ BitVec 32)".into());
        q2.decls.insert("y".into(), "(_ FloatingPoint 8 24)".into());
        q2.assertions.push("(= x #x0000002a)".into());
        q2.assertions.push("(= y (_ -zero 8 24))".into());
        assert_eq!(sess.check_query(&q2), CheckResult::Sat);
        let vals = sess.get_values(&["x".into(), "y".into()]).unwrap();
        assert_eq!(vals[0], SExpr::Atom("#x0000002a".into()));
        assert_eq!(
            vals[1],
            SExpr::List(vec![
                SExpr::Atom("_".into()),
                SExpr::Atom("-zero".into()),
                SExpr::Atom("8".into()),
                SExpr::Atom("24".into()),
            ])
        );
    }
}
