"""A star import can rebind MASK and low when the module runs: nothing here may lift."""
MASK = 0xFF


def low(x):
    return x & MASK


from json import *  # noqa: E402,F403
