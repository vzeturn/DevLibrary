#!/usr/bin/env python3
"""Generates the reference data for dl-qr's tests from segno, an independent encoder.

Dev-only tool, not used by the build. Reproduce with:

    python -m pip install --user segno==1.6.6 qrcode==8.2
    python crates/dl-qr/tests/gen_vectors.py

segno's padding-bit step is replaced by the ISO behaviour (see ``_iso_padding_bits``);
everything else is stock segno. Every case without ECI is also built with python-qrcode,
a third implementation, and must be identical to segno's matrix.

Writes, next to this script:

* ``vectors.txt``: QR matrices built by segno with a forced version, ECC level, segment
  modes and mask, plus segno's penalty scores, used by ``tests/segno.rs`` and the penalty
  unit test in ``src/matrix.rs``.
* ``tables.txt``: segno's block structure (ISO/IEC 18004 Table 9) and alignment pattern
  positions (Annex E) for all versions, used by ``src/tables.rs``.

Record format of ``vectors.txt`` (one field per line, ``end`` closes a case)::

    case <name>
    ecc <L|M|Q|H>
    version <1..40>
    mask <0..7>                     forced mask of the stored matrix
    seg <numeric|alphanumeric|byte> <hex of the bytes>
    seg eci <assignment number>
    text                            optional: QrBuilder::encode_text on the concatenated
                                    segment bytes must pick exactly these segments
    penalty <n1> <n2> <n3> <n4>     segno.encoder.mask_scores() of the stored matrix
    best_mask <k>                   argmin over the 8 final (format info included) matrices
                                    of segno's penalty score, ties -> lowest mask
    segno_mask <k>                  segno's own automatic choice (scores before format
                                    info is drawn, see README note in tests/segno.rs)
    rows <size>
    <size lines of hex, MSB = leftmost module, padded to a multiple of 4 bits>
    end
"""

import os
import random

import segno
from segno import consts, encoder

try:  # Optional second reference (python-qrcode 8.x), used as a cross-check.
    import qrcode
    import qrcode.util as qrutil
except ImportError:  # pragma: no cover
    qrcode = None

HERE = os.path.dirname(os.path.abspath(__file__))


def _iso_padding_bits(buff, version, length):
    """ISO/IEC 18004 7.4.10: pad with zero bits only up to the next codeword boundary.

    segno 1.6.6 always appends ``8 - length % 8`` bits, so a bit stream that is already
    byte aligned after the terminator (every byte-mode-only payload that does not fill the
    symbol) gets an extra 0x00 codeword instead of the first 0xEC pad codeword. The symbol
    still decodes, but is not what the standard (or ZXing, Nayuki, python-qrcode) produces.
    """
    if version not in (consts.VERSION_M1, consts.VERSION_M3) and length % 8:
        buff.extend([0] * (8 - length % 8))


encoder.write_padding_bits = _iso_padding_bits

def tlv(tag, value):
    return "%02d%02d%s" % (tag, len(value), value)


def crc16_ccitt(data: bytes) -> int:
    crc = 0xFFFF
    for byte in data:
        crc ^= byte << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x1021) if crc & 0x8000 else crc << 1
            crc &= 0xFFFF
    return crc


def vietqr_payload():
    """A VietQR (NAPAS / EMVCo merchant-presented) payload, as used for bank transfers."""
    bank = tlv(0, "970436") + tlv(1, "0011012345678")
    merchant = tlv(0, "A000000727") + tlv(1, bank) + tlv(2, "QRIBFTTA")
    payload = (tlv(0, "01") + tlv(1, "12") + tlv(38, merchant) + tlv(53, "704")
               + tlv(54, "1500000") + tlv(58, "VN")
               + tlv(62, tlv(8, "Thanh toan hoa don HD2024000123")) + "6304")
    return payload + "%04X" % crc16_ccitt(payload.encode())


VIETQR = vietqr_payload()


def case(name, segs, ecc, version, mask, text=False, eci_utf8=False):
    return dict(name=name, segs=segs, ecc=ecc, version=version, mask=mask, text=text,
                eci_utf8=eci_utf8)


def b(s):
    return s.encode("utf-8") if isinstance(s, str) else s


def split_runs(text, runs):
    """Splits ``text`` into (mode, substring) segments of the given lengths."""
    segs, pos = [], 0
    for mode, n in runs:
        segs.append((mode, text[pos:pos + n]))
        pos += n
    assert pos == len(text)
    return segs


def _data_bits(mode, n, version):
    """Bit length of one segment of ``n`` characters (ISO/IEC 18004 7.4)."""
    r = 0 if version <= 9 else 1 if version <= 26 else 2
    if mode == "numeric":
        return 4 + (10, 12, 14)[r] + n // 3 * 10 + (0, 4, 7)[n % 3]
    if mode == "alphanumeric":
        return 4 + (9, 11, 13)[r] + n // 2 * 11 + n % 2 * 6
    return 4 + (8, 16, 16)[r] + 8 * n


_FILL = {"numeric": "0123456789", "alphanumeric": "HTTPS://VIETQR.VN/PAY $%*+-.",
         "byte": "abcdefghijklmnopqrstuvwxyz"}


def max_case(mode, ecc, version, mask):
    """A single-segment case that fills ``version``-``ecc`` exactly to its capacity.

    The length is derived from segno's capacity table and confirmed with segno itself: the
    payload fits and one more character does not.
    """
    cap = consts.SYMBOL_CAPACITY[version][encoder.normalize_errorlevel(ecc)]
    n = 0
    while _data_bits(mode, n + 1, version) <= cap:
        n += 1
    fill = _FILL[mode]
    data = (fill * (n // len(fill) + 2))[:n + 1]
    content = data[:n].encode() if mode == "byte" else data[:n]
    segno.make_qr(content, error=ecc, version=version, mode=mode, boost_error=False)
    try:
        over = data.encode() if mode == "byte" else data
        segno.make_qr(over, error=ecc, version=version, mode=mode, boost_error=False)
    except segno.DataOverflowError:
        pass
    else:
        raise SystemExit("max_case %s %s v%d: %d chars is not the capacity" % (mode, ecc,
                                                                              version, n))
    return case("max-v%02d-%s-%s" % (version, ecc, mode), [(mode, data[:n])], ecc, version,
                mask, text=True)


VN = "Thanh toán hóa đơn 123"
rng = random.Random(18004)
BIN_MAX = bytes(rng.randrange(256) for _ in range(2953))

CASES = [
    # Classic examples.
    case("hello-world-1q", [("alphanumeric", "HELLO WORLD")], "q", 1, 6, text=True),
    case("iso-numeric-1m", [("numeric", "01234567")], "m", 1, 2, text=True),
    case("url-2m", [("byte", "https://example.com")], "m", 2, 0, text=True),
    case("url-auto-version-l", [("byte", "https://example.com/checkin?event=42&seat=A7")], "l",
         None, 5, text=True),
    # Vietnamese UTF-8 in byte mode, without and with ECI 26.
    case("vn-utf8-2l", [("byte", VN)], "l", 2, 1, text=True),
    case("vn-utf8-4h", [("byte", VN)], "h", 4, 4, text=True),
    case("vn-utf8-eci-3m", [("eci", 26), ("byte", VN)], "m", 3, 5, eci_utf8=True),
    # Single modes at version 1/2.
    case("numeric-pi-2h", [("numeric", "3141592653589793238462643383279")], "h", 2, 7,
         text=True),
    case("alnum-checkin-2q", [("alphanumeric", "CHECKIN-2024-000123")], "q", 2, 3),
    case("numeric-41-1l", [("numeric", "1" * 41)], "l", 1, 4, text=True),
    case("byte-17-1l", [("byte", "abcdefghijklmnopq")], "l", 1, 0, text=True),
    # Version 7: first version with version information.
    case("byte-7l", [("byte", "Version 7 carries version information blocks.")], "l", 7, 0),
    case("byte-7m", [("byte", "https://example.com/invoice/lookup")], "m", 7, 3, text=True),
    case("alnum-7q", [("alphanumeric", "INVOICE LOOKUP HD-2024/000777")], "q", 7, 5),
    case("numeric-7h", [("numeric", "9876543210" * 9)], "h", 7, 6, text=True),
    # Version 10: character count indicators widen.
    case("byte-10l", [("byte", "x" * 200)], "l", 10, 1, text=True),
    case("numeric-10m", [("numeric", "0123456789" * 30)], "m", 10, 2, text=True),
    case("alnum-10q", [("alphanumeric", "ABC DEF-123/45:6" * 10)], "q", 10, 7),
    case("byte-10h", [("byte", VN * 4)], "h", 10, 4, text=True),
    # Version 20.
    case("byte-20l", [("byte", bytes(range(256)) * 3)], "l", 20, 2),
    case("alnum-20m", [("alphanumeric", "QR CODE $%*+-./: " * 40)], "m", 20, 4),
    case("numeric-20q", [("numeric", "5" * 1000)], "q", 20, 6, text=True),
    case("byte-20h", [("byte", "Check-in ticket #" * 20)], "h", 20, 3),
    # Version 40 at maximum capacity.
    case("byte-max-40l", [("byte", BIN_MAX)], "l", 40, 7),
    case("numeric-max-40h", [("numeric", "".join(str(i % 10) for i in range(3057)))], "h", 40,
         0, text=True),
    case("alnum-max-40m", [("alphanumeric", ("HTTPS://VIETQR.VN/" * 200)[:3391])], "m", 40, 5),
    case("byte-max-40q", [("byte", ("Hóa đơn " * 200).encode("utf-8")[:1663])], "q", 40, 1),
    # Exactly full symbols at versions 1, 2, 7, 10 and 20, every level and mode.
    max_case("alphanumeric", "h", 1, 2),
    max_case("byte", "m", 2, 5),
    max_case("numeric", "q", 2, 1),
    max_case("alphanumeric", "q", 7, 4),
    max_case("byte", "h", 7, 6),
    max_case("numeric", "h", 10, 3),
    max_case("alphanumeric", "l", 10, 0),
    max_case("byte", "m", 20, 7),
    max_case("numeric", "l", 20, 5),
    max_case("alphanumeric", "h", 20, 1),
    max_case("byte", "l", 40, 3),
    # VietQR (EMVCo) payload, smallest version: as one byte segment, and with the mixed
    # segmentation that encode_text picks (asserted in src/segment.rs).
    case("vietqr-byte-m", [("byte", VIETQR)], "m", None, 2),
    case("vietqr-byte-h", [("byte", VIETQR)], "h", None, 6),
    case("vietqr-mixed-m", split_runs(VIETQR, [
        ("numeric", 20), ("alphanumeric", 1), ("numeric", 44), ("alphanumeric", 8),
        ("numeric", 22), ("alphanumeric", 2), ("numeric", 8), ("byte", 21), ("numeric", 16),
        ("alphanumeric", 2)]), "m", None, 4, text=True),
    case("vietqr-mixed-q", split_runs(VIETQR, [
        ("numeric", 20), ("alphanumeric", 1), ("numeric", 44), ("alphanumeric", 8),
        ("numeric", 22), ("alphanumeric", 2), ("numeric", 8), ("byte", 21), ("numeric", 16),
        ("alphanumeric", 2)]), "q", None, 7, text=True),
    # Mixed-mode streams (the optimal segmentation chosen by encode_text).
    case("mixed-alnum-num-byte-2m",
         [("alphanumeric", "INVOICE 2024 "), ("numeric", "000123456789"), ("byte", " ok")],
         "m", 2, 2, text=True),
    case("mixed-byte-num-2l",
         [("byte", "https://x.vn/c?id="), ("numeric", "000201010212263800")], "l", 2, 6,
         text=True),
    case("mixed-eci-byte-num-3q", [("eci", 26), ("byte", "Mã đơn: "), ("numeric", "20240512000001")],
         "q", 3, 0),
]

# One case per version (1..=40), cycling levels and masks, byte mode, auto mask compared.
for v in range(1, 41):
    ecc = "lmqh"[v % 4]
    cap = consts.SYMBOL_CAPACITY[v][encoder.normalize_errorlevel(ecc)] // 8 - 3
    data = bytes((i * 7 + v) % 251 for i in range(cap * 3 // 4))
    CASES.append(case("sweep-v%02d-%s" % (v, ecc), [("byte", data)], ecc, v, (v * 3) % 8))


MODE_CONST = {"numeric": consts.MODE_NUMERIC, "alphanumeric": consts.MODE_ALPHANUMERIC,
              "byte": consts.MODE_BYTE}


def to_segno_content(segs):
    # segno takes (content, mode constant) tuples for multi-segment input.
    content = []
    for mode, data in segs:
        if mode == "eci":
            continue
        if mode == "byte":
            content.append((b(data), MODE_CONST[mode]))
        else:
            content.append((data, MODE_CONST[mode]))
    return content


def make(c, mask, version):
    kw = dict(error=c["ecc"], version=version, mask=mask, boost_error=False)
    content = to_segno_content(c["segs"])
    if c["eci_utf8"] or any(m == "eci" for m, _ in c["segs"]):
        # segno writes ECI 26 before each byte segment whose encoding is not ISO 8859-1.
        content = [(d.decode("utf-8"), m, "utf-8") if m == consts.MODE_BYTE else (d, m)
                   for d, m in content]
        kw["eci"] = True
    return segno.make_qr(content, **kw)


def make_pyqrcode(c, mask, version):
    """Same symbol built by python-qrcode, or None if unavailable / ECI (unsupported)."""
    if qrcode is None or c["eci_utf8"] or any(m == "eci" for m, _ in c["segs"]):
        return None
    levels = {"l": qrcode.constants.ERROR_CORRECT_L, "m": qrcode.constants.ERROR_CORRECT_M,
              "q": qrcode.constants.ERROR_CORRECT_Q, "h": qrcode.constants.ERROR_CORRECT_H}
    modes = {"numeric": qrutil.MODE_NUMBER, "alphanumeric": qrutil.MODE_ALPHA_NUM,
             "byte": qrutil.MODE_8BIT_BYTE}
    q = qrcode.QRCode(version=version, error_correction=levels[c["ecc"]], mask_pattern=mask,
                      border=0)
    for mode, data in c["segs"]:
        q.add_data(qrutil.QRData(b(data), mode=modes[mode]))
    q.make(fit=False)
    return [[1 if m else 0 for m in row] for row in q.get_matrix()]


def matrix_hex(matrix):
    rows = []
    for row in matrix:
        bits = "".join("1" if m else "0" for m in row)
        bits += "0" * (-len(bits) % 4)
        rows.append("%0*x" % (len(bits) // 4, int(bits, 2)))
    return rows


def score(matrix):
    n = len(matrix)
    return encoder.mask_scores(matrix, n, n)


def seg_line(mode, data):
    if mode == "eci":
        return "seg eci %d" % data
    return "seg %s %s" % (mode, b(data).hex())


def main():
    out = ["# Generated by tests/gen_vectors.py with segno %s. Do not edit." % segno.__version__]
    agree = 0
    cross = 0
    for c in CASES:
        try:
            qr = make(c, c["mask"], c["version"])
        except Exception as e:
            raise SystemExit("case %s: %s" % (c["name"], e))
        version = qr.version
        assert qr.mask == c["mask"]
        other = make_pyqrcode(c, c["mask"], version)
        if other is not None:
            if other != [list(row) for row in qr.matrix]:
                raise SystemExit("case %s: segno and python-qrcode disagree" % c["name"])
            cross += 1
        # Every mask, final matrices (format and version info drawn), scored by segno.
        totals = [sum(score(make(c, k, version).matrix)) for k in range(8)]
        best = totals.index(min(totals))
        native = make(c, None, version).mask
        agree += best == native
        out.append("case " + c["name"])
        out.append("ecc " + c["ecc"].upper())
        out.append("version %d" % version)
        out.append("mask %d" % c["mask"])
        out.extend(seg_line(m, d) for m, d in c["segs"])
        if c["text"]:
            out.append("text")
        out.append("penalty %d %d %d %d" % score(qr.matrix))
        out.append("best_mask %d" % best)
        out.append("segno_mask %d" % native)
        out.append("rows %d" % len(qr.matrix))
        out.extend(matrix_hex(qr.matrix))
        out.append("end")
    with open(os.path.join(HERE, "vectors.txt"), "w", newline="\n") as f:
        f.write("\n".join(out) + "\n")

    tables = ["# Generated by tests/gen_vectors.py with segno %s. Do not edit." % segno.__version__,
              "# block <version> <ecc> (<blocks> <total codewords> <data codewords>)+",
              "# align <version> <positions...>"]
    for v in range(1, 41):
        for letter in "LMQH":
            groups = consts.ECC[v][encoder.normalize_errorlevel(letter.lower())]
            fields = " ".join("%d %d %d" % (g.num_blocks, g.num_total, g.num_data) for g in groups)
            tables.append("block %d %s %s" % (v, letter, fields))
    for v in range(1, 41):
        # segno's table starts at version 2; version 1 has no alignment patterns.
        pos = consts.ALIGNMENT_POS[v - 2] if v > 1 else ()
        tables.append(("align %d %s" % (v, " ".join(map(str, pos)))).rstrip())
    with open(os.path.join(HERE, "tables.txt"), "w", newline="\n") as f:
        f.write("\n".join(tables) + "\n")
    print("wrote %d cases; %d also identical in python-qrcode; segno's native mask equals "
          "format-included best mask in %d" % (len(CASES), cross, agree))


if __name__ == "__main__":
    main()
