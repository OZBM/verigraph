#ifndef VG_SHIM_MBEDTLS_SHA256_H
#define VG_SHIM_MBEDTLS_SHA256_H

#include <stdint.h>

typedef struct mbedtls_sha256_context {
    uint32_t total[2];
    uint32_t state[8];
    unsigned char buffer[64];
} mbedtls_sha256_context;

#endif
