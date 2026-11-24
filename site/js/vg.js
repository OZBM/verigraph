// The host side of a `vg build --target wasm32-wasip1` binary, run as it is. The binary reads its
// inputs from stdin in the docs/13 byte encoding, calls its node's checked entry and writes the
// results to stdout; it exits 1 when the inputs break the node's PRE. This file provides the
// handful of WASI calls the binary makes, with stdin and stdout as byte buffers. It does no
// computing of its own.

const EXIT = Symbol("exit");
const EBADF = 8, ESPIPE = 70;

// A WASI command runs once per instance, so each call instantiates the compiled module afresh.
export async function load(url) {
  const module = await WebAssembly.compileStreaming(fetch(url));
  return (input = new Uint8Array(0)) => run(module, input);
}

export async function run(module, input) {
  let memory, pos = 0, code = 0;
  const out = [], err = [];
  const dv = () => new DataView(memory.buffer);
  const iovecs = (iovs, n) =>
    Array.from({ length: n }, (_, i) =>
      new Uint8Array(memory.buffer, dv().getUint32(iovs + 8 * i, true), dv().getUint32(iovs + 8 * i + 4, true)));
  const none = (countPtr, sizePtr) => (dv().setUint32(countPtr, 0, true), dv().setUint32(sizePtr, 0, true), 0);
  const wasi = {
    args_sizes_get: none,
    args_get: () => 0,
    environ_sizes_get: none,
    environ_get: () => 0,
    clock_time_get: (_id, _precision, ptr) => (dv().setBigUint64(ptr, 0n, true), 0),
    fd_close: () => 0,
    fd_fdstat_get(fd, ptr) {
      if (fd > 2) return EBADF;
      new Uint8Array(memory.buffer, ptr, 24).fill(0);
      dv().setUint8(ptr, 2); // a character device
      return 0;
    },
    fd_prestat_get: () => EBADF, // no preopened directories: the binary's sandbox grants none
    fd_prestat_dir_name: () => EBADF,
    fd_seek: () => ESPIPE,
    fd_read(fd, iovs, n, nread) {
      if (fd !== 0) return EBADF;
      let total = 0;
      for (const buf of iovecs(iovs, n)) {
        const k = Math.min(buf.length, input.length - pos);
        buf.set(input.subarray(pos, pos + k));
        pos += k;
        total += k;
        if (k < buf.length) break;
      }
      dv().setUint32(nread, total, true);
      return 0;
    },
    fd_write(fd, iovs, n, nwritten) {
      if (fd !== 1 && fd !== 2) return EBADF;
      let total = 0;
      for (const buf of iovecs(iovs, n)) {
        (fd === 1 ? out : err).push(buf.slice());
        total += buf.length;
      }
      dv().setUint32(nwritten, total, true);
      return 0;
    },
    proc_exit(c) {
      code = c;
      throw EXIT;
    },
  };
  const instance = await WebAssembly.instantiate(module, { wasi_snapshot_preview1: wasi });
  memory = instance.exports.memory;
  try {
    instance.exports._start();
  } catch (e) {
    if (e !== EXIT) throw e;
  }
  if (code !== 0) throw new Error(`exit ${code}: ${new TextDecoder().decode(concat(err))}`);
  return concat(out);
}

function concat(parts) {
  const bytes = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) bytes.set(p, at), at += p.length;
  return bytes;
}

// docs/13 encodings of the inputs the page sends: little-endian scalars, arrays packed in order.
// Types: "f32", "u32", "u64", "u8", "bool", "bytes" (a Uint8Array copied as it is) and "slice"
// (a Uint8Array as a Slice<u8>: its length as a u64, then its bytes).
const WIDTH = { f32: 4, u32: 4, u64: 8, u8: 1, bool: 1 };
export function encode(fields) {
  const size = fields.reduce((n, [type, v]) =>
    n + (type === "bytes" ? v.length : type === "slice" ? 8 + v.length : WIDTH[type] * (Array.isArray(v) ? v.length : 1)), 0);
  const bytes = new Uint8Array(size);
  const view = new DataView(bytes.buffer);
  let at = 0;
  for (const [type, value] of fields) {
    if (type === "bytes" || type === "slice") {
      if (type === "slice") view.setBigUint64(at, BigInt(value.length), true), at += 8;
      bytes.set(value, at), at += value.length;
      continue;
    }
    for (const v of Array.isArray(value) ? value : [value]) {
      if (type === "f32") view.setFloat32(at, v, true);
      else if (type === "u32") view.setUint32(at, v, true);
      else if (type === "u64") view.setBigUint64(at, BigInt(v), true);
      else if (type === "u8") view.setUint8(at, v);
      else if (type === "bool") view.setUint8(at, v ? 1 : 0);
      else throw new Error(`encode: unknown type ${type}`);
      at += WIDTH[type];
    }
  }
  return bytes;
}

export const f32s = (bytes, offset, n) => Array.from({ length: n }, (_, i) => new DataView(bytes.buffer).getFloat32(offset + 4 * i, true));
export const u32 = (bytes, offset) => new DataView(bytes.buffer).getUint32(offset, true);
export const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
