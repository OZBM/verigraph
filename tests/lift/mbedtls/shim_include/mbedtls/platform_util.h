#ifndef VG_SHIM_MBEDTLS_PLATFORM_UTIL_H
#define VG_SHIM_MBEDTLS_PLATFORM_UTIL_H

#include <stddef.h>

static inline void mbedtls_platform_zeroize(void *buf, size_t len) {
    (void)buf;
    (void)len;
}

#endif
