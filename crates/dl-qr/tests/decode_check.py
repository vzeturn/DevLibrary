#!/usr/bin/env python3
"""Decodes the QR codes written by the `qr_dump` example with zxing-cpp.

Dev-only tool, not used by the build:

    python -m pip install --user zxing-cpp numpy
    cargo run -p dl-qr --example qr_dump -- target/qr-decode
    python crates/dl-qr/tests/decode_check.py target/qr-decode

Exits non-zero if any code fails to decode or decodes to a different payload.
"""

import os
import sys

import numpy as np
import zxingcpp

SCALE = 4  # pixels per module


def read_pbm(path):
    with open(path) as f:
        tokens = [t for line in f if not line.startswith("#") for t in line.split()]
    assert tokens[0] == "P1"
    w, h = int(tokens[1]), int(tokens[2])
    bits = "".join(tokens[3:])
    modules = np.array([c == "1" for c in bits[: w * h]], dtype=bool).reshape(h, w)
    img = np.where(modules, 0, 255).astype(np.uint8)
    return np.kron(img, np.ones((SCALE, SCALE), dtype=np.uint8))


def decode(img):
    """Returns (result, detector name) for the first error-free QR result, or (None, None).

    The normal (camera) detector is tried first. zxing-cpp's ``valid`` flag also requires
    non-empty content, so an empty symbol is accepted when it decodes without error. The
    camera detector does not find the empty symbol (its data area is nothing but the
    repeating pad codewords 0xEC 0x11); the pure-barcode detector, meant for synthetic
    images like these, decodes it with no error.
    """
    for detector, pure in (("camera", False), ("pure", True)):
        results = zxingcpp.read_barcodes(img, formats=zxingcpp.BarcodeFormat.QRCode,
                                         return_errors=True, is_pure=pure)
        for r in results:
            if not r.error:
                return r, detector
    return None, None


def main():
    folder = sys.argv[1] if len(sys.argv) > 1 else "target/qr-decode"
    failures = 0
    total = 0
    with open(os.path.join(folder, "manifest.txt"), encoding="utf-8") as f:
        entries = [line.split() for line in f if line.strip()]
    for name, version, ecc, mask, hexdata in entries:
        total += 1
        expected = b"" if hexdata == "-" else bytes.fromhex(hexdata)
        img = read_pbm(os.path.join(folder, name + ".pbm"))
        r, detector = decode(img)
        if r is None:
            print("FAIL %-22s v%-2s %-8s mask %s: not decoded" % (name, version, ecc, mask))
            failures += 1
            continue
        got = r.text.encode("utf-8")
        # The manifest spells the level out ("Medium"); zxing reports its letter.
        ok = got == expected and r.ec_level == ecc[0]
        status = "ok  " if ok else "FAIL"
        print("%s %-22s v%-2s %-8s mask %s: %4d bytes, zxing ec=%s, %s detector"
              % (status, name, version, ecc, mask, len(got), r.ec_level, detector))
        if not ok:
            print("     expected %r\n     got      %r" % (expected[:80], got[:80]))
            failures += 1
    print("%d/%d decoded correctly" % (total - failures, total))
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
