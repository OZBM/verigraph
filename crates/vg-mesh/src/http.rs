//! Std-only HTTP/1.1 API server for VG-Mesh (P7.5).
//!
//! Endpoints:
//! - `POST /query` (`MeshQueryIn` JSON -> `MeshQueryOut` JSON)
//! - `POST /publish` (`{"vgir_hex", "signer", "signature", ...}` -> `PublishOut` JSON)
//! - `GET /checkpoint` (C2SP signed checkpoint text)
//! - `GET /tile/<L>/<N>[.p/<W>]` (raw tile bytes)

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value as J, json};
use vg_core::ir::parse_hex;

use crate::base64;
use crate::store::Mesh;
use crate::types::{MeshContractQuery, MeshQueryOut, Submission};

/// Maximum accepted HTTP request body size (16 MiB).
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Serves the VG-Mesh HTTP/1.1 API on `listener` until an unrecoverable accept error occurs.
pub fn serve(mesh: Mesh, listener: TcpListener) -> std::io::Result<()> {
    let shared = Arc::new(Mutex::new(mesh));
    let shutdown = Arc::new(AtomicBool::new(false));
    serve_shared(shared, listener, shutdown)
}

/// Serves a shared `Mesh` until `shutdown` becomes `true`.
pub fn serve_shared(
    mesh: Arc<Mutex<Mesh>>,
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                handle_connection(&mesh, stream);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

struct HttpResponse {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(status: u16, reason: &'static str, val: &J) -> Self {
        Self {
            status,
            reason,
            content_type: "application/json",
            body: serde_json::to_vec(val).unwrap_or_else(|_| b"{}".to_vec()),
        }
    }

    fn bad_request(msg: &str) -> Self {
        Self::json(400, "Bad Request", &json!({ "error": msg }))
    }

    fn not_found(msg: &str) -> Self {
        Self::json(404, "Not Found", &json!({ "error": msg }))
    }

    fn write_to<W: Write>(&self, stream: &mut W) -> std::io::Result<()> {
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.status,
            self.reason,
            self.content_type,
            self.body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}

fn handle_connection(mesh: &Arc<Mutex<Mesh>>, stream: TcpStream) {
    handle_stream(mesh, stream);
}

/// Processes a single HTTP/1.1 request/response cycle on any `Read + Write` stream.
pub fn handle_stream<S: Read + Write>(mesh: &Arc<Mutex<Mesh>>, mut stream: S) {
    let resp = match read_request(&mut stream) {
        Ok(req) => dispatch(mesh, req),
        Err(resp) => resp,
    };
    let _ = resp.write_to(&mut stream);
}

struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request<R: Read>(stream: &mut R) -> Result<HttpRequest, HttpResponse> {
    let mut reader = BufReader::new(stream);
    let mut req_line = String::new();
    let n = reader
        .read_line(&mut req_line)
        .map_err(|e| HttpResponse::bad_request(&format!("read request line: {e}")))?;
    if n == 0 || req_line.len() > MAX_HEADER_BYTES {
        return Err(HttpResponse::bad_request("empty or oversized request line"));
    }
    let req_line_trim = req_line.trim_end_matches(['\r', '\n']);
    let mut parts = req_line_trim.split(' ');
    let method = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| HttpResponse::bad_request("missing HTTP method"))?
        .to_string();
    let raw_target = parts
        .next()
        .filter(|s| s.starts_with('/'))
        .ok_or_else(|| HttpResponse::bad_request("invalid request target"))?;
    let version = parts
        .next()
        .filter(|s| *s == "HTTP/1.1" || *s == "HTTP/1.0")
        .ok_or_else(|| HttpResponse::bad_request("unsupported HTTP version"))?;
    let _ = version;
    if parts.next().is_some() {
        return Err(HttpResponse::bad_request("malformed request line"));
    }

    let path = raw_target
        .split_once('?')
        .map_or(raw_target, |(p, _)| p)
        .to_string();

    let mut total_header_bytes = req_line.len();
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|e| HttpResponse::bad_request(&format!("read header: {e}")))?;
        if bytes == 0 {
            return Err(HttpResponse::bad_request("unexpected EOF in headers"));
        }
        total_header_bytes += bytes;
        if total_header_bytes > MAX_HEADER_BYTES {
            return Err(HttpResponse::bad_request("headers exceed limit"));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        let (name, val) = trimmed
            .split_once(':')
            .ok_or_else(|| HttpResponse::bad_request("malformed header line"))?;
        let lname = name.trim().to_ascii_lowercase();
        let val = val.trim();
        if lname == "content-length" {
            let parsed: usize = val
                .parse()
                .map_err(|_| HttpResponse::bad_request("invalid Content-Length"))?;
            if parsed > MAX_BODY_BYTES {
                return Err(HttpResponse::bad_request("request body exceeds 16 MiB"));
            }
            if content_length.is_some_and(|prev| prev != parsed) {
                return Err(HttpResponse::bad_request("conflicting Content-Length"));
            }
            content_length = Some(parsed);
        } else if lname == "transfer-encoding" {
            return Err(HttpResponse::bad_request(
                "Transfer-Encoding is not supported; send Content-Length",
            ));
        }
    }

    let len = content_length.unwrap_or(0);
    if len > MAX_BODY_BYTES {
        return Err(HttpResponse::bad_request("request body exceeds 16 MiB"));
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|_| HttpResponse::bad_request("truncated request body"))?;
    }

    Ok(HttpRequest { method, path, body })
}

fn dispatch(mesh: &Arc<Mutex<Mesh>>, req: HttpRequest) -> HttpResponse {
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/query") => handle_query(mesh, &req.body),
        ("POST", "/publish") => handle_publish(mesh, &req.body),
        ("GET", "/checkpoint") => handle_checkpoint(mesh),
        ("GET", p) if p.starts_with("/tile/") => handle_tile(mesh, &p[1..]),
        _ => HttpResponse::not_found("unknown endpoint"),
    }
}

fn handle_query(mesh: &Arc<Mutex<Mesh>>, body: &[u8]) -> HttpResponse {
    let val: J = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return HttpResponse::bad_request(&format!("invalid JSON: {e}")),
    };
    let q = match MeshContractQuery::from_json(&val) {
        Ok(q) => q,
        Err(e) => return HttpResponse::bad_request(&e.to_string()),
    };
    let limit = q.limit.unwrap_or(16);
    let guard = match mesh.lock() {
        Ok(g) => g,
        Err(_) => return HttpResponse::bad_request("mesh lock poisoned"),
    };
    match guard.query(&q, limit) {
        Ok(candidates) => HttpResponse::json(200, "OK", &MeshQueryOut { candidates }.to_json()),
        Err(e) => HttpResponse::bad_request(&e.to_string()),
    }
}

fn decode_hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid hex string".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte =
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("bad hex byte: {e}"))?;
        out.push(byte);
    }
    Ok(out)
}

fn decode_sig_64(s: &str) -> Result<[u8; 64], String> {
    let trimmed = s.trim();
    if trimmed.len() == 128 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        let bytes = decode_hex_bytes(trimmed)?;
        return bytes
            .try_into()
            .map_err(|_| "signature must be 64 bytes".to_string());
    }
    let bytes = base64::decode(trimmed)?;
    bytes
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())
}

fn handle_publish(mesh: &Arc<Mutex<Mesh>>, body: &[u8]) -> HttpResponse {
    let val: J = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return HttpResponse::bad_request(&format!("invalid JSON: {e}")),
    };
    let Some(obj) = val.as_object() else {
        return HttpResponse::bad_request("publish body must be a JSON object");
    };
    let Some(vgir_hex) = obj.get("vgir_hex").and_then(J::as_str) else {
        return HttpResponse::bad_request("missing string field vgir_hex");
    };
    let vgir = match decode_hex_bytes(vgir_hex) {
        Ok(b) => b,
        Err(e) => return HttpResponse::bad_request(&format!("vgir_hex: {e}")),
    };
    let Some(signer) = obj.get("signer").and_then(J::as_str) else {
        return HttpResponse::bad_request("missing string field signer");
    };

    let claimed_hash = match obj.get("claimed_hash").and_then(J::as_str) {
        Some(s) => match parse_hex(s.trim()) {
            Some(h) => Some(h),
            None => return HttpResponse::bad_request("claimed_hash must be 64 hex chars"),
        },
        None => None,
    };

    let signature: Option<[u8; 64]> = match obj.get("signature") {
        None | Some(J::Null) => None,
        Some(J::String(s)) if s.trim().is_empty() => None,
        Some(J::String(s)) => match decode_sig_64(s) {
            Ok(sig) => Some(sig),
            Err(e) => return HttpResponse::bad_request(&format!("signature: {e}")),
        },
        Some(_) => return HttpResponse::bad_request("signature must be a string or null"),
    };

    let label = obj
        .get("label")
        .and_then(J::as_str)
        .map(ToString::to_string);
    let intent = obj
        .get("intent")
        .and_then(J::as_str)
        .map(ToString::to_string);
    let spec_name = obj
        .get("spec_name")
        .and_then(J::as_str)
        .map(ToString::to_string);

    let mut guard = match mesh.lock() {
        Ok(g) => g,
        Err(_) => return HttpResponse::bad_request("mesh lock poisoned"),
    };

    let public_key = match obj.get("public_key").and_then(J::as_str) {
        Some(pk_hex) => match parse_hex(pk_hex.trim()) {
            Some(pk) => pk,
            None => return HttpResponse::bad_request("public_key must be 64 hex chars"),
        },
        None => match guard.get_signer(signer) {
            Ok(Some(pk)) => pk,
            _ => [0u8; 32],
        },
    };

    let sub = Submission {
        vgir,
        claimed_hash,
        label,
        intent,
        spec_name,
        signer: signer.to_string(),
        public_key,
        signature,
        provenance: None,
    };
    let out = guard.publish(sub);
    HttpResponse::json(200, "OK", &out.to_json())
}

fn handle_checkpoint(mesh: &Arc<Mutex<Mesh>>) -> HttpResponse {
    let guard = match mesh.lock() {
        Ok(g) => g,
        Err(_) => return HttpResponse::bad_request("mesh lock poisoned"),
    };
    let note = guard.checkpoint_note();
    HttpResponse {
        status: 200,
        reason: "OK",
        content_type: "text/plain; charset=utf-8",
        body: note.into_bytes(),
    }
}

fn handle_tile(mesh: &Arc<Mutex<Mesh>>, rel_path: &str) -> HttpResponse {
    let guard = match mesh.lock() {
        Ok(g) => g,
        Err(_) => return HttpResponse::bad_request("mesh lock poisoned"),
    };
    match guard.read_tile(rel_path) {
        Ok(Some(bytes)) => HttpResponse {
            status: 200,
            reason: "OK",
            content_type: "application/octet-stream",
            body: bytes,
        },
        Ok(None) => HttpResponse::not_found("tile not found"),
        Err(e) => HttpResponse::bad_request(&e.to_string()),
    }
}
