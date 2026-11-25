#version 450
// OmniLift GLSL ingest kernels (W7): integer hash and RNG functions common in shaders.
precision highp int;

uint bitmix(uint a, uint b) {
    return ((a << 5) ^ (b >> 3)) + (a & 0xffu);
}

// PCG hash (Jarzynski and Olano, JCGT 2020).
uint pcg_hash(uint v) {
    uint state = v * 747796405u + 2891336453u;
    uint word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// Polynomial string-style hash over 0..n-1; GLSL int arithmetic wraps on overflow.
int poly_hash(int n) {
    int s = 7;
    for (int i = 0; i < n; i++) {
        s = s * 31 + i;
    }
    return s;
}
