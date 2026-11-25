// OmniLift Zig ingest kernels (W7). `export` gives C symbols; `+%` is Zig's wrapping add.
export fn bitmix(a: u32, b: u32) u32 {
    return ((a << 5) ^ (b >> 3)) +% (a & 0xff);
}

export fn sum_bytes(buf: [*]const u8, len: usize) u64 {
    var s: u64 = 0;
    var i: usize = 0;
    while (i < len) : (i += 1) {
        s +%= buf[i];
    }
    return s;
}
