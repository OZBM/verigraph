# Lift sources (P8, G8)

Third-party functions that OmniLift lifts, vendored unchanged so the lift is reproducible offline.
Each folder keeps its upstream license.

| Folder | Upstream | Files | License |
| --- | --- | --- | --- |
| zlib/ | zlib 1.4, tarball sha256 334dd5ad7add012826a4cf33a5365492611abc78ed5027cf26f66bb3e7b4c34a (Homebrew source fetch) | crc32.c, crc32.h, adler32.c, zutil.h, zlib.h, zconf.h | zlib |
| mbedtls/ | Mbed TLS 4.3.0, tarball sha256 6124cd0f05dcde1e350432881be91794907b55b5d1e33dc938297302f1956914, tf-psa-crypto/drivers/builtin/src/sha256.c | sha256.c | Apache-2.0 OR GPL-2.0-or-later, used under Apache-2.0 |
| languages/ | written for VeriGraph (W7, 2026-11-25) | kernels.cpp, kernels.swift, kernels.zig, kernels.glsl | MIT OR Apache-2.0, as the repository |
| python/ | written for VeriGraph (R-13, 2026-11-25) | kernels.py, rejected.py, star_import.py | MIT OR Apache-2.0, as the repository |
| rust-sha2/ | crates.io sha2 0.10.9, src/sha256/soft.rs and src/consts.rs (the portable backend) | soft.rs, consts.rs | MIT OR Apache-2.0 |
