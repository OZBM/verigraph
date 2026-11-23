/* Non-modifying wrapper beside vendored Mbed TLS sha256.c for OmniLift. */
#include "sha256.c"

void mbedtls_sha256_compress(uint32_t state[8], const unsigned char data[64]) {
    mbedtls_sha256_context ctx;
    for (int i = 0; i < 8; i++) {
        ctx.state[i] = state[i];
    }
    mbedtls_internal_sha256_process(&ctx, data);
    for (int i = 0; i < 8; i++) {
        state[i] = ctx.state[i];
    }
}
