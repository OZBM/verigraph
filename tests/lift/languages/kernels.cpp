// OmniLift C++ ingest kernels (W7). Entry points are extern "C" so their symbols are unmangled.
#include <cstddef>
#include <cstdint>

namespace {
constexpr uint32_t rotl(uint32_t v, unsigned s) { return (v << s) | (v >> (32u - s)); }
}

extern "C" uint32_t bitmix(uint32_t a, uint32_t b) {
    return ((a << 5) ^ (b >> 3)) + (a & 0xffu);
}

extern "C" uint64_t sum_bytes(const uint8_t *buf, size_t len) {
    uint64_t s = 0;
    for (size_t i = 0; i < len; i++) {
        s += buf[i];
    }
    return s;
}

extern "C" uint32_t rotmix(uint32_t a, uint32_t b) {
    return rotl(a, 7) ^ rotl(b, 13);
}
