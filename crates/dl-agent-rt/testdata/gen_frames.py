#!/usr/bin/env python3
"""Generates frames.txt, the dl-rt/1 golden vectors, from the byte layout in
docs/protocol/dl-rt-1.md. Independent of the Rust code: it packs bytes with struct.

Run from anywhere:  python3 crates/dl-agent-rt/testdata/gen_frames.py
"""

import os
import struct

CHAN = {"control": 0, "capture": 1, "commands": 2, "results": 3, "bulk": 4}
KIND = {
    "data": 1,
    "ack": 2,
    "credit": 3,
    "resume": 4,
    "resume_ok": 5,
    "ping": 6,
    "pong": 7,
    "close": 8,
}
U64MAX = 2**64 - 1


def header(chan, kind, seq, sent_us, payload_len, flags=0, version=1):
    # version u8 | chan u8 | kind u8 | flags u8 | seq u64 | sent_us u64 | payload_len u32, big endian
    return struct.pack(
        ">BBBBQQI", version, chan, kind, flags, seq, sent_us, payload_len
    )


def ext_area(entries):
    body = b"".join(struct.pack(">BQ", tag, value) for tag, value in entries)
    return struct.pack(">B", len(body)) + body


def frame(chan, kind, seq, sent_us, payload=b"", ext=None):
    c, k = CHAN[chan], KIND[kind]
    if ext:
        return (
            header(c, k, seq, sent_us, len(payload), flags=1) + ext_area(ext) + payload
        )
    return header(c, k, seq, sent_us, len(payload)) + payload


def fields(
    chan, kind, seq, sent_us, payload=b"", t_detected=None, t_read=None, extra=()
):
    parts = [
        "version=1",
        f"chan={chan}",
        f"kind={kind}",
        f"seq={seq}",
        f"sent_us={sent_us}",
        f"payload={payload.hex()}",
    ]
    if t_detected is not None:
        parts.append(f"t_detected_us={t_detected}")
    if t_read is not None:
        parts.append(f"t_read_us={t_read}")
    parts.extend(extra)
    return " ".join(parts)


def close_payload(code, reason):
    return struct.pack(">H", code) + reason


vectors = []


def ok(
    name,
    chan,
    kind,
    seq,
    sent_us,
    payload=b"",
    ext=None,
    extra=(),
    mode=None,
    max_frame=None,
):
    known = dict(ext or [])
    raw = frame(chan, kind, seq, sent_us, payload, ext)
    v = {
        "name": name,
        "hex": raw.hex(),
        "fields": fields(
            chan, kind, seq, sent_us, payload, known.get(1), known.get(2), extra
        ),
    }
    if mode:
        v["mode"] = mode
    if max_frame:
        v["max_frame"] = str(max_frame)
    vectors.append(v)


def err(name, raw, error, max_frame=None):
    v = {"name": name, "hex": raw.hex(), "error": error}
    if max_frame:
        v["max_frame"] = str(max_frame)
    vectors.append(v)


# ---- valid frames (checked both ways: decode == fields, encode(fields) == hex) ----
ok("data_capture_basic", "capture", "data", 1, 1000, b"abc")
ok("data_bulk_empty_payload", "bulk", "data", 7, 0)
ok("data_control_config_push", "control", "data", 3, 99, b'{"cfg":1}')
ok(
    "data_results_ext_both",
    "results",
    "data",
    0x0102030405060708,
    1764150000000000,
    b"{}",
    ext=[(1, 1764149999990000), (2, 1764149999995000)],
)
ok("data_capture_ext_read_only", "capture", "data", 2, 20, b"x", ext=[(2, 15)])
ok("data_commands_max_values", "commands", "data", U64MAX, U64MAX, b"\xff")
ok("data_at_limit", "capture", "data", 1, 0, b"\x00" * 8, max_frame=32)
ok("ack_capture", "capture", "ack", 42, 5)
ok("ack_control_zero", "control", "ack", 0, 0)
ok(
    "credit_commands_1mib",
    "commands",
    "credit",
    0,
    7,
    struct.pack(">Q", 1048576),
    extra=["credit=1048576"],
)
ok("credit_bulk_zero", "bulk", "credit", 0, 7, struct.pack(">Q", 0), extra=["credit=0"])
ok("resume_results", "results", "resume", 17, 1000)
ok("resume_ok_capture", "capture", "resume_ok", 16, 1001)
ok("ping_empty", "control", "ping", 1, 123456)
ok("ping_payload", "control", "ping", 2, 123456, bytes([0, 1, 2, 3]))
ok("pong_payload", "control", "pong", 2, 123456, bytes([0, 1, 2, 3]))
ok(
    "close_normal_bye",
    "control",
    "close",
    0,
    9,
    close_payload(0, b"bye"),
    extra=["close_code=0", "close_reason=" + b"bye".hex()],
)
ok(
    "close_going_away_no_text",
    "control",
    "close",
    0,
    9,
    close_payload(1, b""),
    extra=["close_code=1", "close_reason="],
)
ok(
    "close_unknown_code_utf8",
    "control",
    "close",
    0,
    9,
    close_payload(0xBEEF, "hết".encode()),
    extra=["close_code=48879", "close_reason=" + "hết".encode().hex()],
)
ok(
    "close_reason_123_bytes",
    "control",
    "close",
    0,
    9,
    close_payload(2, b"r" * 123),
    extra=["close_code=2", "close_reason=" + (b"r" * 123).hex()],
)
# Unknown extension tags are skipped, so this one only decodes to these fields.
ok(
    "data_ext_unknown_tag_skipped",
    "capture",
    "data",
    5,
    50,
    b"z",
    ext=[(9, 1), (1, 5)],
    mode="decode",
)

# ---- malformed frames (decode must fail with this error name) ----
good = frame("capture", "data", 1, 1000, b"abc")
err("err_empty", b"", "truncated")
err("err_version_0", b"\x00" + good[1:], "version")
err("err_version_2_short", b"\x02", "version")
err("err_header_23_bytes", good[:23], "truncated")
err("err_channel_5", header(5, 1, 1, 0, 0), "channel")
err("err_kind_0", header(1, 0, 1, 0, 0), "kind")
err("err_kind_9", header(1, 9, 1, 0, 0), "kind")
err("err_flags_reserved_bit", header(1, 1, 1, 0, 0, flags=0x02), "flags")
err(
    "err_flags_high_bit_with_ext",
    header(1, 1, 1, 0, 0, flags=0x81) + ext_area([(1, 1)]),
    "flags",
)
err("err_ext_missing_len_byte", header(1, 1, 1, 0, 0, flags=1), "truncated")
err("err_ext_len_zero", header(1, 1, 1, 0, 0, flags=1) + b"\x00", "ext")
err(
    "err_ext_len_not_multiple_of_9",
    header(1, 1, 1, 0, 0, flags=1) + b"\x08" + b"\x01" * 8,
    "ext",
)
err(
    "err_ext_entries_truncated",
    header(1, 1, 1, 0, 0, flags=1) + b"\x09" + b"\x01\x00\x00\x00\x00",
    "truncated",
)
err("err_ext_tag_zero", header(1, 1, 1, 0, 0, flags=1) + ext_area([(0, 1)]), "ext")
err(
    "err_ext_duplicate_tag",
    header(1, 1, 1, 0, 0, flags=1) + ext_area([(2, 1), (2, 2)]),
    "ext",
)
err("err_payload_truncated", header(1, 1, 1, 0, 4) + b"abc", "truncated")
err("err_payload_len_huge", header(1, 1, 1, 0, 0xFFFFFFFF) + b"abc", "truncated")
err("err_trailing_byte", header(1, 1, 1, 0, 1) + b"ab", "trailing")
err(
    "err_oversize",
    frame("capture", "data", 1, 0, b"\x00" * 9),
    "oversize",
    max_frame=32,
)
err("err_data_seq_zero", frame("capture", "data", 0, 0, b"a"), "field")
err("err_ack_with_payload", frame("capture", "ack", 1, 0, b"a"), "field")
err("err_resume_with_payload", frame("capture", "resume", 1, 0, b"a"), "field")
err("err_resume_ok_with_payload", frame("capture", "resume_ok", 1, 0, b"a"), "field")
err("err_ext_on_ack", frame("capture", "ack", 1, 0, ext=[(1, 1)]), "field")
err(
    "err_credit_seq_nonzero",
    frame("capture", "credit", 1, 0, struct.pack(">Q", 5)),
    "field",
)
err(
    "err_credit_payload_7_bytes", frame("capture", "credit", 0, 0, b"\x00" * 7), "field"
)
err("err_ping_on_capture", frame("capture", "ping", 1, 0), "field")
err("err_pong_on_results", frame("results", "pong", 1, 0), "field")
err("err_close_on_bulk", frame("bulk", "close", 0, 0, close_payload(0, b"")), "field")
err(
    "err_close_seq_nonzero",
    frame("control", "close", 1, 0, close_payload(0, b"")),
    "field",
)
err("err_close_payload_1_byte", frame("control", "close", 0, 0, b"\x00"), "field")
err(
    "err_close_reason_not_utf8",
    frame("control", "close", 0, 0, close_payload(0, b"\xff\xfe")),
    "field",
)
err(
    "err_close_reason_124_bytes",
    frame("control", "close", 0, 0, close_payload(0, b"r" * 124)),
    "field",
)

out = [
    "# dl-rt/1 golden frames. Generated by gen_frames.py from docs/protocol/dl-rt-1.md;",
    "# edit the script, not this file. Blocks are separated by a blank line.",
    "# Keys: name, hex (the whole frame), fields (decoded) or error (decode error name),",
    "# optional max_frame (limit for this vector, default 1048576) and mode",
    '# ("decode": the encoder is not expected to reproduce the bytes).',
    "",
]
for v in vectors:
    for key in ("name", "max_frame", "mode", "hex", "fields", "error"):
        if key in v:
            out.append(f"{key}: {v[key]}".rstrip())
    out.append("")
path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "frames.txt")
with open(path, "w", encoding="utf-8", newline="\n") as f:
    f.write("\n".join(out))
print(f"{len(vectors)} vectors -> {path}")
