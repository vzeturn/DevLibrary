/*
 * dl_ffi.h - C ABI of DevLibrary (https://github.com/vzeturn/DevLibrary), ABI version 1.
 *
 * Conventions for every function:
 *  - Text in and out is UTF-8 as pointer + length, no NUL terminator. A NULL pointer is
 *    allowed only with length 0.
 *  - Results go into a caller-owned buffer out[0..cap). *out_len always receives the size
 *    of the complete result. If it does not fit, nothing is written and
 *    DL_ERR_BUFFER_TOO_SMALL is returned: retry with a buffer of *out_len bytes.
 *  - On DL_ERR_INVALID_INPUT the output is a UTF-8 message explaining the problem.
 *  - No function allocates memory for the caller, keeps state, or is unsafe to call from
 *    several threads at once.
 */
#ifndef DL_FFI_H
#define DL_FFI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DL_FFI_ABI_VERSION 1u

#define DL_OK                    0
#define DL_ERR_BUFFER_TOO_SMALL  1
#define DL_ERR_NULL_POINTER      2
#define DL_ERR_INVALID_UTF8      3
#define DL_ERR_INVALID_INPUT     4

/* Number reading options (dl_vn_number_words, dl_vn_amount_words). */
#define DL_WORDS_LE        (1u << 0) /* "lẻ" instead of "linh" */
#define DL_WORDS_NGAN      (1u << 1) /* "ngàn" instead of "nghìn" */
#define DL_WORDS_TU        (1u << 2) /* "tư" instead of "bốn" after "mươi" */
#define DL_WORDS_TI        (1u << 3) /* "tỉ" instead of "tỷ" */
#define DL_WORDS_UNSIGNED  (1u << 4) /* no "Âm" for negative amounts */
#define DL_WORDS_CHAN      (1u << 5) /* append " chẵn" */

/* VietQR options (dl_vietqr_payload). */
#define DL_VIETQR_DYNAMIC    (1u << 0) /* one-payment code (01 = "12") */
#define DL_VIETQR_CARD       (1u << 1) /* transfer to card (QRIBFTTC) */
#define DL_VIETQR_FOLD_TEXT  (1u << 2) /* remove diacritics, ASCII only, max 25 chars */

uint32_t dl_ffi_abi_version(void);

/* 1050000 -> "một triệu không trăm năm mươi nghìn" */
int32_t dl_vn_number_words(uint64_t n, uint32_t flags,
                           uint8_t *out, size_t cap, size_t *out_len);

/* 1050000, "đồng" -> "Một triệu không trăm năm mươi nghìn đồng"; every int64. */
int32_t dl_vn_amount_words(int64_t amount, const uint8_t *unit, size_t unit_len, uint32_t flags,
                           uint8_t *out, size_t cap, size_t *out_len);

/* "Bàn 5" -> "Ban 5" */
int32_t dl_vn_remove_diacritics(const uint8_t *text, size_t text_len,
                                uint8_t *out, size_t cap, size_t *out_len);

/* NAPAS VietQR payload with CRC. amount in đồng, 0 = no amount. bill/purpose may be empty. */
int32_t dl_vietqr_payload(const uint8_t *bin, size_t bin_len,
                          const uint8_t *account, size_t account_len,
                          uint64_t amount, uint32_t flags,
                          const uint8_t *bill, size_t bill_len,
                          const uint8_t *purpose, size_t purpose_len,
                          uint8_t *out, size_t cap, size_t *out_len);

/* DL_OK if the payload is a valid VietQR (fields and CRC), else the reason. */
int32_t dl_vietqr_verify(const uint8_t *payload, size_t payload_len,
                         uint8_t *out, size_t cap, size_t *out_len);

#ifdef __cplusplus
}
#endif

#endif /* DL_FFI_H */
