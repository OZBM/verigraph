// OmniLift Swift ingest kernels (W7). @_cdecl gives C symbols; arithmetic uses Swift's
// wrapping operators, because the trapping ones lower to a trap that OmniLift rejects.
@_cdecl("bitmix")
public func bitmix(_ a: UInt32, _ b: UInt32) -> UInt32 {
    return ((a << 5) ^ (b >> 3)) &+ (a & 0xff)
}

@_cdecl("sum_bytes")
public func sum_bytes(_ buf: UnsafePointer<UInt8>, _ len: Int) -> UInt64 {
    var s: UInt64 = 0
    var i = 0
    while i < len {
        s &+= UInt64(buf[i])
        i &+= 1
    }
    return s
}
