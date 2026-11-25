# Security policy

## Supported versions

DevLibrary is at version 0.1. Fixes are made on the `main` branch; pin a commit or tag
and update when an advisory is published.

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Report them privately with
[GitHub security advisories](https://github.com/vzeturn/devlibrary/security/advisories/new)
("Report a vulnerability"). Include the crate, the commit, a description of the impact
and, if possible, a proof of concept.

You will get an acknowledgement within 7 days. Once a fix is ready, it is released
together with an advisory that credits you, unless you prefer otherwise.

## Scope and status of the cryptographic code

`dl-hash`, `dl-rand`, `dl-jwt` and `dl-otp` implement published standards and pass their
official test vectors, but they **have not been independently audited**. In particular:

- the code aims to be constant-time with respect to secret data (MAC and code
  comparison use `dl_hash::ct_eq`), but this is not verified at the machine-code level
  and there is no hardening against physical side channels;
- secret zeroing in `dl-otp` and `dl-jwt` is best effort (no `unsafe`, no volatile
  writes);
- SHA-1 and MD5 are provided for protocols that require them, not for new designs.

Reports about panics on untrusted input, incorrect parsing (for example a token or
payload accepted when it should be rejected), or deviations from a cited standard are
treated as security issues.
