"""Generates the reference vectors used by src/tests.rs from Python's datetime module.

Run with Python 3.11+:  python crates/dl-time/testdata/gen_vectors.py
Outputs dates.txt and unix.txt. The output is deterministic (fixed seed).
"""

import datetime as dt
import os
import random

os.chdir(os.path.dirname(os.path.abspath(__file__)))

EPOCH_ORDINAL = dt.date(1970, 1, 1).toordinal()

rng = random.Random(20261115)

# --- dates.txt: y m d unix_days weekday(Mon=0) iso_year iso_week iso_weekday yday
dates = set()
for y in (1, 2, 4, 99, 100, 400, 1582, 1600, 1899, 1900, 1969, 1970, 1999, 2000,
          2004, 2020, 2021, 2024, 2025, 2026, 2027, 2100, 2400, 9998, 9999):
    for m, d in ((1, 1), (1, 2), (1, 3), (1, 4), (2, 28), (3, 1), (12, 28), (12, 29),
                 (12, 30), (12, 31)):
        dates.add(dt.date(y, m, d))
    if y % 4 == 0 and (y % 100 != 0 or y % 400 == 0):
        dates.add(dt.date(y, 2, 29))
while len(dates) < 500:
    dates.add(dt.date.fromordinal(rng.randint(1, dt.date.max.toordinal())))

with open("dates.txt", "w", newline="\n") as f:
    f.write("# y m d unix_days weekday(Mon=0) iso_year iso_week iso_weekday yday\n")
    for d in sorted(dates):
        iso = d.isocalendar()
        f.write(f"{d.year} {d.month} {d.day} {d.toordinal() - EPOCH_ORDINAL} {d.weekday()} "
                f"{iso.year} {iso.week} {iso.weekday} {d.timetuple().tm_yday}\n")

# --- unix.txt: unix_seconds rfc3339_utc http_date
lo = int(dt.datetime(1, 1, 1, tzinfo=dt.timezone.utc).timestamp())
hi = int(dt.datetime(9999, 12, 31, 23, 59, 59, tzinfo=dt.timezone.utc).timestamp())
secs = {0, -1, 1, 86399, 86400, -86400, -86401, 951782400, 1763176830, 2147483647,
        -2147483648, 253402300799, -62135596800, 784111777}
while len(secs) < 300:
    secs.add(rng.randint(lo, hi))
for _ in range(100):
    secs.add(rng.randint(-2**31, 2**31))

with open("unix.txt", "w", newline="\n") as f:
    f.write("# unix_seconds rfc3339_utc http_date\n")
    for s in sorted(secs):
        t = dt.datetime(1970, 1, 1, tzinfo=dt.timezone.utc) + dt.timedelta(seconds=s)
        # Built by hand: strftime's %Y padding and %a/%b names vary by platform and locale.
        hms = f"{t.hour:02d}:{t.minute:02d}:{t.second:02d}"
        rfc = f"{t.year:04d}-{t.month:02d}-{t.day:02d}T{hms}Z"
        wd = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][t.weekday()]
        mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
               "Dec"][t.month - 1]
        http = f"{wd}, {t.day:02d} {mon} {t.year:04d} {hms} GMT"
        f.write(f"{s} {rfc} {http}\n")
