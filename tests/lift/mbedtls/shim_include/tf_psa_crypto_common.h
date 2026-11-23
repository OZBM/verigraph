#ifndef VG_SHIM_TF_PSA_CRYPTO_COMMON_H
#define VG_SHIM_TF_PSA_CRYPTO_COMMON_H

#include <stdint.h>
#include <stddef.h>

#define MBEDTLS_SHA256_C 1

#define MBEDTLS_GET_UINT32_BE(data, offset) ( \
    ((uint32_t)(data)[(offset)] << 24) | \
    ((uint32_t)(data)[(offset) + 1] << 16) | \
    ((uint32_t)(data)[(offset) + 2] << 8) | \
    ((uint32_t)(data)[(offset) + 3]) )

#define MBEDTLS_PUT_UINT32_BE(n, data, offset) do { \
    (data)[(offset)]     = (uint8_t)((n) >> 24); \
    (data)[(offset) + 1] = (uint8_t)((n) >> 16); \
    (data)[(offset) + 2] = (uint8_t)((n) >> 8); \
    (data)[(offset) + 3] = (uint8_t)(n); \
} while (0)

#endif
