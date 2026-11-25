"""Functions outside the R-13 subset: the front end must reject each, never guess."""

counter = 0
TABLE = [1, 2, 3]
TABLE.append(4)


def true_div(a, b):
    return a / b


def recurse(n):
    return 0 if n == 0 else recurse(n - 1)


def uses_mutated_table(i):
    return TABLE[i]


def maybe_unbound(c):
    if c:
        y = 1
    return y


def slices(data: bytes):
    return len(data[1:])


def bool_from_int(x) -> bool:
    return x + 1


def bare_return(x):
    return


def float_const(x):
    return x + 1.5


def mutates(xs: list[int]):
    xs[0] = 1
    return 0


def side_effect(x):
    print(x)
    return x
