"""Generates vectors.txt, the reference test vectors for dl-decimal.

Run:  python gen_vectors.py   (writes vectors.txt next to this script; deterministic seed)

Every expected value is computed with Python's `decimal` module under an explicit
context (prec=400 for exact intermediates, then `quantize` with the rounding mode the
Rust code documents) and then cross-checked against a second, independent
implementation on `fractions.Fraction` / Python big integers. Add, subtract and (when
the scale stays <= 28) multiply are also checked against the plain Python context
prec=38, ROUND_HALF_EVEN, Emax=37, which is the C#-like rule the crate implements;
so are division (when 38 significant digits bind before 28 decimal places) and
remainder (whenever Python's prec=38 remainder is defined).

Requires only the Python standard library (tested with Python 3.13). The output is
deterministic: the same script always writes the same file.

Line format (space separated, decimals printed like dl-decimal's Display):
  add|sub|mul|div|rem  A B  RESULT|overflow|divzero
  round A DP MODE RESULT
  divr  A B DP MODE RESULT|overflow|divzero
  mult  A STEP MODE RESULT|overflow
  pct   A RATE RESULT|overflow
  pow   A N RESULT|overflow
  cmp   A B -1|0|1
  ps    STRING RESULT|overflow|scale          (strict FromStr)
  pr    STRING MODE RESULT|overflow           (parse_rounded)
  f2d   F64_BITS_HEX RESULT|overflow|scale    (TryFrom<f64>)
  d2f   A F64_BITS_HEX                        (to_f64)
"""

import os
import random
import struct
from decimal import (
    Decimal as D, Context, Inexact, InvalidOperation, DivisionByZero, Overflow,
    ROUND_HALF_EVEN, ROUND_HALF_UP, ROUND_HALF_DOWN, ROUND_UP, ROUND_DOWN,
    ROUND_CEILING, ROUND_FLOOR,
)
from fractions import Fraction as F

BIG = Context(prec=400, rounding=ROUND_HALF_EVEN, Emax=10**6, Emin=-10**6,
              traps=[InvalidOperation, DivisionByZero, Overflow])
C38 = Context(prec=38, rounding=ROUND_HALF_EVEN, Emax=37, Emin=-10**6,
              traps=[InvalidOperation, DivisionByZero])
MAXM = 10**38 - 1
MODES = {
    'he': ROUND_HALF_EVEN, 'hu': ROUND_HALF_UP, 'hd': ROUND_HALF_DOWN, 'up': ROUND_UP,
    'dn': ROUND_DOWN, 'ce': ROUND_CEILING, 'fl': ROUND_FLOOR,
}
R = random.Random(20250601)
OUT = []


# ---------- representation helpers ----------

def fmt(m, s):
    """dl-decimal Display of mantissa m, scale s."""
    neg = m < 0
    t = str(abs(m))
    if s > 0:
        if len(t) <= s:
            t = '0' * (s - len(t) + 1) + t
        t = t[:-s] + '.' + t[-s:]
    return ('-' if neg and m != 0 else '') + t


def dec(v):
    m, s = v
    return D(f"{m}E-{s}")


def tup(x):
    sign, digits, e = x.as_tuple()
    m = int(''.join(map(str, digits)))
    return (-m if sign else m, -e)


def fr(v):
    m, s = v
    return F(m, 10**s)


def show(r):
    return r if isinstance(r, str) else fmt(*r)


# ---------- reference A: decimal module ----------

def fit(x, mode=ROUND_HALF_EVEN):
    """Round exact x once to the largest scale <= min(own scale, 28) with <= 38 digits."""
    sign, digits, e = x.as_tuple()
    if x.is_zero():
        return (0, min(max(-e, 0), 28))
    target = max(min(e, 0), -28, x.adjusted() - 37)
    if target > 0:
        return None
    m, s = tup(x.quantize(D(1).scaleb(target), rounding=mode, context=BIG))
    if abs(m) > MAXM:
        assert abs(m) == 10**38
        if s == 0:
            return None
        m, s = m // 10, s - 1
    return (m, s)


def strip(m, s):
    if m == 0:
        return (0, 0)
    while s > 0 and m % 10 == 0:
        m, s = m // 10, s - 1
    return (m, s)


def dec_div(a, b):
    if b[0] == 0:
        return 'divzero'
    if a[0] == 0:
        return (0, 0)
    q = BIG.divide(dec(a), dec(b))
    t = min(28, 37 - q.adjusted())
    if t < 0:
        return 'overflow'
    m, s = tup(q.quantize(D(1).scaleb(-t), rounding=ROUND_HALF_EVEN, context=BIG))
    if abs(m) > MAXM:
        if s == 0:
            return 'overflow'
        m, s = m // 10, s - 1
    return strip(m, s)


def dec_divr(a, b, dp, mode):
    if b[0] == 0:
        return 'divzero'
    q = BIG.divide(dec(a), dec(b)).quantize(D(1).scaleb(-dp), rounding=mode, context=BIG)
    m, s = tup(q)
    return 'overflow' if abs(m) > MAXM else (m, s)


def dec_rem(a, b):
    if b[0] == 0:
        return 'divzero'
    return tup(BIG.remainder(dec(a), dec(b)))


def dec_round(a, dp, mode):
    if dp >= a[1]:
        return a
    return tup(dec(a).quantize(D(1).scaleb(-dp), rounding=mode, context=BIG))


def dec_mult(a, step, mode):
    st = BIG.abs(dec(step))  # builtin abs() would round to the default 28 digits
    n = BIG.divide(dec(a), st).quantize(D(1), rounding=mode, context=BIG)
    m, s = tup(BIG.multiply(n, st))
    while abs(m) > MAXM and s > 0 and m % 10 == 0:
        m, s = m // 10, s - 1
    return 'overflow' if abs(m) > MAXM else (m, s)


def strict(x):
    """FromStr: the written scale and digits are kept exactly, never rounded.

    Errors, in this order: more than 38 integer digits -> overflow; scale (fraction
    digits minus exponent) above 28 -> scale; more than 38 written digits -> overflow.
    """
    sign, digits, e = x.as_tuple()
    scale = max(-e, 0)
    if x.is_zero():
        return 'scale' if scale > 28 else (0, scale)
    if x.adjusted() + 1 > 38:
        return 'overflow'
    if scale > 28:
        return 'scale'
    m = int(''.join(map(str, digits))) * 10**max(e, 0)
    if len(str(m)) > 38:
        return 'overflow'
    return (-m if sign else m, scale)


# ---------- reference B: fractions / big integers ----------

def rnd_int(q, mode, neg):
    """Round the non-negative Fraction q to an integer; neg is the sign of the value."""
    n, r = divmod(q.numerator, q.denominator)
    d = q.denominator
    half = (2 * r > d) - (2 * r < d)
    if r == 0:
        return n
    up = {
        ROUND_HALF_EVEN: half > 0 or (half == 0 and n % 2 == 1),
        ROUND_HALF_UP: half >= 0,
        ROUND_HALF_DOWN: half > 0,
        ROUND_UP: True,
        ROUND_DOWN: False,
        ROUND_CEILING: not neg,
        ROUND_FLOOR: neg,
    }[mode]
    return n + up


def floor_log10(q):
    """floor(log10(q)) for a positive Fraction."""
    e = len(str(q.numerator)) - len(str(q.denominator))
    while F(10)**e > q:
        e -= 1
    while F(10)**(e + 1) <= q:
        e += 1
    return e


def ffit(x, scale, mode=ROUND_HALF_EVEN):
    """x is exact with `scale` decimals (x * 10^scale is an integer)."""
    neg = x < 0
    if x == 0:
        return (0, min(scale, 28))
    n = abs(x) * 10**scale
    assert n.denominator == 1
    n = n.numerator
    digits = len(str(n))
    k = max(scale - 28, digits - 38, 0)
    if k > scale:
        return None
    q = rnd_int(F(n, 10**k), mode, neg)
    s = scale - k
    if q > MAXM:
        if s == 0:
            return None
        q, s = q // 10, s - 1
    return (-q if neg else q, s)


def frac_div(a, b):
    if b[0] == 0:
        return 'divzero'
    if a[0] == 0:
        return (0, 0)
    q = fr(a) / fr(b)
    neg = q < 0
    adj = floor_log10(abs(q))
    if adj >= 38:
        return 'overflow'
    t = min(28, 37 - adj)
    m = rnd_int(abs(q) * 10**t, ROUND_HALF_EVEN, neg)
    if m > MAXM:
        if t == 0:
            return 'overflow'
        m, t = m // 10, t - 1
    return strip(-m if neg else m, t)


def frac_divr(a, b, dp, mode):
    if b[0] == 0:
        return 'divzero'
    q = fr(a) / fr(b)
    neg = q < 0
    m = rnd_int(abs(q) * 10**dp, mode, neg)
    return 'overflow' if m > MAXM else (-m if neg else m, dp)


def frac_rem(a, b):
    if b[0] == 0:
        return 'divzero'
    x, y = fr(a), fr(b)
    q = abs(x) // abs(y)
    r = abs(x) - q * abs(y)
    if x < 0:
        r = -r
    s = max(a[1], b[1])
    m = r * 10**s
    assert m.denominator == 1
    return (m.numerator, s)


def frac_mult(a, step, mode):
    st = abs(fr(step))
    x = fr(a)
    n = rnd_int(abs(x) / st, mode, x < 0)
    m, s = n * abs(step[0]), step[1]
    while m > MAXM and s > 0 and m % 10 == 0:
        m, s = m // 10, s - 1
    return 'overflow' if m > MAXM else ((-m if x < 0 else m), s)


def same(r1, r2):
    """Same representation (treat -0 like 0)."""
    if isinstance(r1, str) or isinstance(r2, str) or r1 is None or r2 is None:
        return r1 == r2
    return r1 == r2


CHECKS = {}


def check(name, r1, r2, *args):
    assert same(r1, r2), (name, args, r1, r2)
    CHECKS[name] = CHECKS.get(name, 0) + 1


# ---------- operations with both references ----------

def op_add(a, b, sub=False):
    if sub:
        b = (-b[0], b[1])
    x = BIG.add(dec(a), dec(b))
    r = fit(x)
    r2 = ffit(fr(a) + fr(b), max(a[1], b[1]))
    check('add', r, r2, a, b)
    # Native Python context with the C#-like rule.
    C38.clear_flags()
    y = C38.add(dec(a), dec(b))
    r3 = None if C38.flags[Overflow] else tup(y)
    if r3 is not None and r3[0] == 0:
        r3 = (0, r3[1])
    check('add-c38', r, r3, a, b)
    return 'overflow' if r is None else r


def op_mul(a, b):
    x = BIG.multiply(dec(a), dec(b))
    r = fit(x)
    check('mul', r, ffit(fr(a) * fr(b), a[1] + b[1]), a, b)
    if a[1] + b[1] <= 28:
        C38.clear_flags()
        y = C38.multiply(dec(a), dec(b))
        r3 = None if C38.flags[Overflow] else tup(y)
        if r3 is not None and r3[0] == 0:
            r3 = (0, r3[1])
        check('mul-c38', r, r3, a, b)
    return 'overflow' if r is None else r


def from_c38(y):
    """(mantissa, scale) of a C38 result, with a negative exponent folded into the
    mantissa."""
    m, s = tup(y)
    if s < 0:
        m, s = m * 10**(-s), 0
    return (0, s) if m == 0 else (m, s)


def op_div(a, b):
    r = dec_div(a, b)
    check('div', r, frac_div(a, b), a, b)
    # When the quotient is >= 10^9, 38 significant digits bind before 28 places, so
    # the result is exactly Python's own prec=38 division, normalized.
    if b[0] != 0 and a[0] != 0 and floor_log10(abs(fr(a) / fr(b))) >= 9:
        C38.clear_flags()
        y = C38.divide(dec(a), dec(b))
        r3 = 'overflow' if C38.flags[Overflow] else strip(*from_c38(C38.normalize(y)))
        check('div-c38', r, r3, a, b)
    return r


def op_rem(a, b):
    r = dec_rem(a, b)
    if not isinstance(r, str) and r[0] == 0:
        r = (0, r[1])
    check('rem', r, frac_rem(a, b), a, b)
    # Python's remainder is exact; at prec=38 it refuses (DivisionImpossible) when the
    # integer quotient has more than 38 digits, so compare only when it succeeds.
    if b[0] != 0:
        try:
            r3 = from_c38(C38.remainder(dec(a), dec(b)))
        except InvalidOperation:
            r3 = None
        if r3 is not None:
            check('rem-c38', r, r3, a, b)
    return r


def op_divr(a, b, dp, mode):
    r = dec_divr(a, b, dp, mode)
    if not isinstance(r, str) and r[0] == 0:
        r = (0, r[1])
    check('divr', r, frac_divr(a, b, dp, mode), a, b, dp, mode)
    return r


def op_round(a, dp, mode):
    r = dec_round(a, dp, mode)
    if r[0] == 0:
        r = (0, r[1])
    if dp < a[1]:
        x = fr(a)
        m = rnd_int(abs(x) * 10**dp, mode, x < 0)
        check('round', r, (-m if x < 0 else m, dp), a, dp, mode)
    return r


def op_mult(a, step, mode):
    r = dec_mult(a, step, mode)
    if not isinstance(r, str) and r[0] == 0:
        r = (0, r[1])
    check('mult', r, frac_mult(a, step, mode), a, step, mode)
    return r


def op_pct(a, rate):
    x = BIG.divide(BIG.multiply(dec(a), dec(rate)), D(100))
    # Dividing by 100 must keep the ideal exponent e1 + e2 - 2.
    x = x.quantize(D(1).scaleb(-(a[1] + rate[1] + 2)), context=BIG)
    r = fit(x)
    check('pct', r, ffit(fr(a) * fr(rate) / 100, a[1] + rate[1] + 2), a, rate)
    return 'overflow' if r is None else r


def op_pow(a, n):
    """Binary exponentiation with fit() after every multiplication (documented rule)."""
    acc, base, e = (1, 0), a, n
    while e > 0:
        if e & 1:
            acc = fit(BIG.multiply(dec(acc), dec(base)))
            if acc is None:
                return 'overflow'
        e >>= 1
        if e > 0:
            base = fit(BIG.multiply(dec(base), dec(base)))
            if base is None:
                return 'overflow'
    # Whenever the exact power is representable, the result must be exact.
    exact = fr(a) ** n
    single = ffit(exact, a[1] * n)
    if single is not None and fr(single) == exact:
        assert fr(acc) == exact, (a, n, acc)
    return (0, acc[1]) if acc[0] == 0 else acc


# ---------- input generation ----------

EDGES = [
    (MAXM, 0), (-MAXM, 0), (MAXM, 28), (-MAXM, 28), (MAXM - 1, 0), (1, 28), (-1, 28),
    (5, 28), (-5, 28), (0, 0), (0, 2), (0, 28), (1, 0), (-1, 0), (5, 1), (-5, 1),
    (10**37, 0), (MAXM, 10), (15, 1), (25, 1), (-35, 1), (10**28, 28), (10**38 - 5, 1),
    (5 * 10**37, 0), (3, 0), (7, 0), (2, 0), (99_999_999_999_999_999_999_999_999_9, 28),
    (123_456_789_012_345_678_901_234_567_890_123_456_78, 0),
    (999_999_999_999_999_999_999_999_999_95, 28),
]


def rand_mant(nd):
    return R.randint(0, 9) if nd == 1 else R.randint(10**(nd - 1), 10**nd - 1)


def rand_dec():
    c = R.random()
    if c < 0.35:
        nd, s = R.randint(1, 38), R.randint(0, 28)
    elif c < 0.65:
        nd, s = R.randint(1, 15), R.randint(0, 4)
    elif c < 0.85:
        nd, s = R.randint(1, 12), R.randint(10, 28)
    elif c < 0.93:
        # Mantissas ending in 5: produce exact ties after rounding.
        nd, s = R.randint(2, 38), R.randint(1, 28)
        m = rand_mant(nd) // 10 * 10 + 5
        return (m if R.random() < 0.5 else -m, s)
    else:
        return R.choice(EDGES)
    m = rand_mant(nd)
    return (m if R.random() < 0.5 else -m, s)


def emit(*parts):
    OUT.append(' '.join(str(p) for p in parts))


CRAFTED = [
    # add/sub: carries into the 39th digit, ties at the 38-digit boundary
    ((MAXM, 0), (5, 1)), ((MAXM - 1, 0), (5, 1)), ((MAXM, 0), (4, 1)), ((MAXM, 0), (1, 0)),
    ((MAXM, 28), (1, 28)), ((MAXM, 28), (5, 29 - 1)), ((-MAXM, 0), (-1, 0)),
    ((10**37, 0), (5, 1)), ((10**37 + 1, 0), (5, 1)), ((MAXM, 1), (5, 2)),
    ((1, 28), (-1, 28)), ((15, 1), (-15, 1)), ((MAXM, 0), (-MAXM, 0)),
]

for a, b in CRAFTED:
    emit('add', fmt(*a), fmt(*b), show(op_add(a, b)))
    emit('sub', fmt(*a), fmt(*b), show(op_add(a, b, sub=True)))
for _ in range(700):
    a, b = rand_dec(), rand_dec()
    emit('add', fmt(*a), fmt(*b), show(op_add(a, b)))
for _ in range(700):
    a, b = rand_dec(), rand_dec()
    emit('sub', fmt(*a), fmt(*b), show(op_add(a, b, sub=True)))

MUL_CRAFTED = [
    ((1, 28), (1, 28)), ((5, 28), (1, 1)), ((15, 28), (1, 1)), ((25, 28), (1, 1)),
    ((MAXM, 0), (2, 0)), ((MAXM, 0), (1, 0)), ((10**19, 0), (10**19, 0)),
    ((10**19 - 1, 0), (10**19 + 1, 0)), ((MAXM, 28), (MAXM, 28)), ((MAXM, 14), (MAXM, 14)),
    ((-MAXM, 0), (1, 1)), ((5, 1), (5, 1)), ((125, 3), (8, 0)), ((-15, 1), (15, 1)),
]
for a, b in MUL_CRAFTED:
    emit('mul', fmt(*a), fmt(*b), show(op_mul(a, b)))
for _ in range(700):
    a, b = rand_dec(), rand_dec()
    emit('mul', fmt(*a), fmt(*b), show(op_mul(a, b)))

DIV_CRAFTED = [
    ((1, 0), (3, 0)), ((2, 0), (3, 0)), ((1, 0), (8, 0)), ((10, 0), (4, 0)), ((250, 2), (1, 0)),
    ((1, 28), (2, 0)), ((3, 28), (2, 0)), ((5, 28), (2, 0)), ((MAXM, 0), (2, 0)),
    ((MAXM, 0), (1, 1)), ((MAXM, 0), (MAXM, 0)), ((1, 0), (MAXM, 0)), ((1, 28), (MAXM, 0)),
    ((MAXM, 0), (1, 28)), ((1, 0), (0, 0)), ((0, 5), (7, 0)), ((-7, 0), (2, 0)),
    ((10**38 - 1, 0), (10**38 - 3, 0)), ((10**38 - 2, 0), (2, 1)), ((100000, 0), (3, 0)),
    ((-1, 0), (7, 0)), ((22, 0), (7, 0)), ((1, 0), (10**10, 0)), ((MAXM, 28), (3, 0)),
]
for a, b in DIV_CRAFTED:
    emit('div', fmt(*a), fmt(*b), show(op_div(a, b)))
for _ in range(700):
    a, b = rand_dec(), rand_dec()
    emit('div', fmt(*a), fmt(*b), show(op_div(a, b)))

for a, b in [((-7, 0), (3, 0)), ((7, 0), (-3, 0)), ((75, 1), (2, 0)), ((10, 0), (30, 2)),
             ((MAXM, 0), (1, 28)), ((1, 28), (MAXM, 0)), ((MAXM, 28), (7, 0)), ((5, 0), (0, 3))]:
    emit('rem', fmt(*a), fmt(*b), show(op_rem(a, b)))
for _ in range(700):
    a, b = rand_dec(), rand_dec()
    emit('rem', fmt(*a), fmt(*b), show(op_rem(a, b)))

# Rounding: every mode on positive/negative ties and non-ties.
ROUND_VALUES = [
    '2.5', '1.5', '-2.5', '-1.5', '2.4', '-2.4', '2.6', '-2.6', '0.5', '-0.5', '0.4', '-0.4',
    '1.25', '-1.25', '1.35', '-1.35', '2.345', '-2.345', '1.005', '-1.005', '0.125', '-0.125',
    '99.995', '-99.995', '1.2501', '-1.2499', '7', '-7', '0.0000000000000000000000000005',
    '-0.0000000000000000000000000015', '9999999999.9999999999999999999999999999',
    '-9999999999.9999999999999999999999999999', '99999999999999999999999999999999999999.5',
    '123456.785', '-123456.785', '0.000', '1.000',
]
for v in ROUND_VALUES:
    a = tup(D(v))
    if abs(a[0]) > MAXM:
        continue
    for dp in range(0, 4):
        for name, mode in MODES.items():
            emit('round', fmt(*a), dp, name, show(op_round(a, dp, mode)))
for _ in range(400):
    a = rand_dec()
    dp = R.randint(0, max(a[1], 1))
    name = R.choice(list(MODES))
    emit('round', fmt(*a), dp, name, show(op_round(a, dp, MODES[name])))

for _ in range(500):
    a, b = rand_dec(), rand_dec()
    if R.random() < 0.3:
        b = (R.choice([3, 7, 12, 30, 1000, 6, 9]), 0)
    dp = R.choice([0, 0, 1, 2, 2, 2, 3, 4, R.randint(0, 28)])
    name = R.choice(list(MODES))
    emit('divr', fmt(*a), fmt(*b), dp, name, show(op_divr(a, b, dp, MODES[name])))

STEPS = [(500, 0), (1000, 0), (100, 0), (5, 2), (5, 1), (1, 2), (25, 2), (50, 0), (10, 0),
         (1, 0), (1, 3), (3, 0), (75, 1), (-500, 0), (1, 28)]
for _ in range(400):
    a = rand_dec()
    step = R.choice(STEPS) if R.random() < 0.8 else rand_dec()
    if step[0] == 0:
        step = (1, 0)
    name = R.choice(list(MODES))
    emit('mult', fmt(*a), fmt(*step), name, show(op_mult(a, step, MODES[name])))

RATES = [(10, 0), (8, 0), (5, 0), (0, 0), (5, 1), (125, 1), (15, 0), (3, 2)]
for _ in range(250):
    a = rand_dec()
    rate = R.choice(RATES) if R.random() < 0.8 else rand_dec()
    emit('pct', fmt(*a), fmt(*rate), show(op_pct(a, rate)))

for _ in range(200):
    nd = R.randint(1, 8)
    a = (rand_mant(nd) * R.choice([1, -1]), R.randint(0, 4))
    n = R.randint(0, 14) if R.random() < 0.9 else R.randint(15, 200)
    emit('pow', fmt(*a), n, show(op_pow(a, n)))
for a, n in [((11, 1), 2), ((15, 1), 3), ((1001, 3), 365), ((2, 0), 126), ((2, 0), 127),
             ((10, 0), 37), ((10, 0), 38), ((0, 0), 0), ((0, 2), 3), ((-1, 0), 7), ((5, 1), 100)]:
    emit('pow', fmt(*a), n, show(op_pow(a, n)))

for _ in range(400):
    a = rand_dec()
    c = R.random()
    if c < 0.3 and a[1] < 28 and abs(a[0]) * 10 <= MAXM:
        k = R.randint(1, 28 - a[1])
        while abs(a[0]) * 10**k > MAXM:
            k -= 1
        b = (a[0] * 10**k, a[1] + k)
    elif c < 0.5:
        b = (a[0] + R.choice([-1, 1]), a[1]) if abs(a[0]) < MAXM else (a[0], a[1])
    else:
        b = rand_dec()
    x, y = dec(a), dec(b)
    emit('cmp', fmt(*a), fmt(*b), (x > y) - (x < y))


# ---------- parsing ----------

def rand_number_string():
    sign = R.choice(['', '', '-', '+'])
    lead = '0' * R.choice([0, 0, 0, 1, 3])
    nint = R.choice([0, 1, 2, 5, 10, 20, 30, 38, 39, 45])
    nfrac = R.choice([0, 1, 2, 4, 10, 20, 27, 28, 29, 30, 40])
    if nint == 0 and nfrac == 0:
        nint = 1
    int_part = lead + ''.join(R.choice('0123456789') for _ in range(nint))
    frac = ''.join(R.choice('0123456789') for _ in range(nfrac))
    if R.random() < 0.3:
        frac += '0' * R.randint(1, 12)
    if R.random() < 0.2 and frac:
        frac = frac[:-1] + '5'
    s = sign + int_part
    if frac or R.random() < 0.2:
        s += '.' + frac
    if R.random() < 0.25:
        s += R.choice('eE') + R.choice(['', '+', '-']) + str(R.randint(0, 45))
    return s


for _ in range(600):
    s = rand_number_string()
    x = D(s)
    emit('ps', s, show(strict(x)))
for _ in range(600):
    s = rand_number_string()
    name = R.choice(list(MODES))
    r = fit(D(s), MODES[name])
    if r is not None and r[0] == 0:
        r = (0, r[1])
    emit('pr', s, name, show('overflow' if r is None else r))


# ---------- floats ----------

def bits(f):
    return struct.unpack('<Q', struct.pack('<d', f))[0]


def rand_float():
    c = R.random()
    if c < 0.3:
        return round(R.uniform(-1e7, 1e7), R.randint(0, 4))
    if c < 0.6:
        return R.uniform(-1, 1) * 10 ** R.uniform(-32, 40)
    if c < 0.7:
        return R.choice([0.1, 0.2, 0.3, 0.1 + 0.2, 1e-28, 1e-29, 1.5e-28, 1e38, 9.99e37,
                         1e37, -0.0, 0.0, 5e-324, 1.7976931348623157e308, 2.0**100, 19.99,
                         1234567.89, 0.07, 100.0])
    while True:
        f = struct.unpack('<d', struct.pack('<Q', R.getrandbits(64)))[0]
        if f == f and abs(f) != float('inf'):
            return f


def shortest(f):
    """Exact value of repr(f) with the scale Rust's `format!("{}", f)` shows
    (no trailing fractional zeros, never a negative scale)."""
    x = D(repr(f)).normalize(BIG)
    if x.as_tuple().exponent > 0:
        x = x.quantize(D(1), context=BIG)
    return x


for _ in range(400):
    f = rand_float()
    emit('f2d', f"{bits(f):016x}", show(strict(shortest(f))))
for _ in range(300):
    a = rand_dec()
    emit('d2f', fmt(*a), f"{bits(float(dec(a))):016x}")

with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'vectors.txt'), 'w',
          newline='\n') as fh:
    fh.write('# Generated by gen_vectors.py; do not edit.\n')
    fh.write('\n'.join(OUT) + '\n')
print(len(OUT), 'vectors; cross-checks passed:', dict(sorted(CHECKS.items())))
