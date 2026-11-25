"""OmniLift Python front-end kernels (R-13): each probes one CPython semantic edge.

The lift must return what CPython returns, or Err where CPython raises or a value leaves
int64; tests/lift/python/check.py compares the two on generated inputs.
"""

POLY = 0xEDB88320
SQUARES = (0, 1, 4, 9, 16, 25, 36, 49)


def floordiv_mod(a, b):
    # Python rounds toward negative infinity; C truncates.
    return (a // b) * 1000 + (a % b)


def shifts(a, s):
    # Negative counts raise ValueError; >> floors; << grows until it leaves int64.
    return (a >> s) ^ (a << s)


def power(base, e):
    return base**e


def crc_byte(crc):
    # zipfile._gen_crc: eight constant-trip rounds.
    for _ in range(8):
        if crc & 1:
            crc = (crc >> 1) ^ POLY
        else:
            crc >>= 1
    return crc


def square_at(i):
    # Negative indices count from the end; out of range raises IndexError.
    return SQUARES[i]


def checked_div(a, b):
    q, r = divmod(a, b)
    assert 0 <= r < b or b <= r <= 0
    return q


def sign(x: int) -> int:
    if x < 0:
        return -1
    if x > 0:
        return 1
    return 0


def clamp_or_raise(x, lo, hi):
    if lo > hi:
        raise ValueError("empty range")
    return min(max(x, lo), hi)


def is_power_of_two(n) -> bool:
    return n > 0 and n & (n - 1) == 0


def either(a, b):
    # `or` returns an operand, not a bool.
    return a or b


def fib(n):
    a, b = 0, 1
    for _ in range(n):
        a, b = b, a + b
    return a


def sum_bytes(data: bytes) -> int:
    total = 0
    for byte in data:
        total += byte
    return total


def dot(xs: list[int], ys: list[int]) -> int:
    s = 0
    for i in range(len(xs)):
        s += xs[i] * ys[i]
    return s


def bit_len(x):
    return x.bit_length()


def flag_value(f: bool, x):
    return x if f else -x
