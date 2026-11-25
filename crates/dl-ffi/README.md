# dl-ffi

A C ABI over DevLibrary, so that .NET, Go (cgo), Flutter and WebAssembly hosts call the
Rust implementation directly instead of porting it. The business rules exist once, in
Rust; a host only needs a thin binding.

Part of [DevLibrary](https://github.com/vzeturn/devlibrary).

## Output

| Target | File | Size (release, `opt-level = "z"`, LTO) |
|---|---|---|
| Windows x64 | `dl_ffi.dll` | ~100 KB |
| Linux | `libdl_ffi.so` | |
| macOS | `libdl_ffi.dylib` | |
| Browser / Node | `dl_ffi.wasm` | ~20 KB |
| Static linking (cgo, iOS) | `libdl_ffi.a` / `dl_ffi.lib` | |

```sh
cargo build -p dl-ffi --profile min-size                          # native
cargo build -p dl-ffi --profile min-size --target wasm32-unknown-unknown
```

The header is [`include/dl_ffi.h`](include/dl_ffi.h).

## Conventions

- Text is UTF-8 as pointer + length, no NUL terminator; a null pointer only with length 0.
- Results go into a caller-owned buffer. `*out_len` always receives the size of the
  complete result; if it does not fit, nothing is written and `DL_ERR_BUFFER_TOO_SMALL`
  is returned. A 512-byte stack buffer is enough for every function except a VietQR
  payload with long fields, which never exceeds 512 bytes either.
- `DL_ERR_INVALID_INPUT` writes the reason as UTF-8 into the same buffer.
- No function allocates memory for the caller, keeps state between calls, or panics
  across the boundary. All functions are safe to call from several threads.
- `dl_ffi_abi_version()` returns 1. It changes only when an existing function changes.

## Functions

| Function | Example |
|---|---|
| `dl_vn_number_words(n, flags, ...)` | `1050000` → "một triệu không trăm năm mươi nghìn" |
| `dl_vn_amount_words(amount, unit, flags, ...)` | `1050000, "đồng"` → "Một triệu không trăm năm mươi nghìn đồng"; `-500000` → "Âm năm trăm nghìn đồng" |
| `dl_vn_remove_diacritics(text, ...)` | "Bàn 5" → "Ban 5" |
| `dl_vietqr_payload(bin, account, amount, flags, bill, purpose, ...)` | NAPAS VietQR string with CRC |
| `dl_vietqr_verify(payload, ...)` | checks every field and the CRC |

Flags: `DL_WORDS_LE` ("lẻ"), `DL_WORDS_NGAN`, `DL_WORDS_TU`, `DL_WORDS_TI`,
`DL_WORDS_UNSIGNED` (no "Âm"), `DL_WORDS_CHAN`; `DL_VIETQR_DYNAMIC`, `DL_VIETQR_CARD`,
`DL_VIETQR_FOLD_TEXT` (remove diacritics, keep printable ASCII, collapse spaces, cut to 25
characters, as field 62 requires).

## Example (C)

```c
#include "dl_ffi.h"

uint8_t buf[512];
size_t len;
if (dl_vn_amount_words(1050000, (const uint8_t *)"\xc4\x91\xe1\xbb\x93ng", 7, 0,
                       buf, sizeof buf, &len) == DL_OK) {
    fwrite(buf, 1, len, stdout); /* Một triệu không trăm năm mươi nghìn đồng */
}
```

## Safety

This is the only crate in the workspace that allows `unsafe`, and only to read and write
the caller's buffers. Every block has a `SAFETY:` comment, and the tests call each
function through the C ABI with null pointers, short buffers and invalid UTF-8.

```rust
// The exported functions are also plain Rust functions.
assert_eq!(dl_ffi::dl_ffi_abi_version(), dl_ffi::DL_FFI_ABI_VERSION);
```

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.
