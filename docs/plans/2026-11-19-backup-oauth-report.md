# B2b report: Google Drive and OneDrive uploaders (dl-agent-backup)

Branch `feat/dl-agent-backup-oauth`. RT-0164 task 557 step 4.

## Ported
- `upload/oauth.rs`: shared refresh-token flow (cached access token with 60 s margin, one renew on 401, `Retry-After` aware bounded waits for 429/503, max 5), `invalid_grant` / `invalid_client` / `unauthorized_client` -> `backup_credential_error`.
- `upload/gdrive.rs`: folder resolve/create, multipart upload up to 5 MiB, resumable upload in 32 MiB chunks (multiples of 256 KiB), ranged download, delete. Path `gdrive://<folder>/<name>` as in Go.
- `upload/onedrive.rs`: PUT under 4 MiB, one session up to 20 MiB, above that up to four parallel part sessions plus `<name>.odmanifest` (same layout as Go, so Go-written manifests download). Chunks are multiples of 320 KiB. Download follows the 302 to the pre-authenticated URL without Authorization. Path `onedrive://<path>`.
- `Kind::GoogleDrive` / `Kind::OneDrive` replace `Kind::Later`; only unknown providers return `backup_destination_unsupported`. Scrubber gets `client_secret=`.
- Base and token URLs are constants; only `for_test` (cfg(test)) can change them.

## Differences from Go
- Smaller chunks (Drive 32 MiB vs 64, OneDrive 10 MiB vs 60) to bound memory with four parallel sessions.
- Real resume: Drive status query (`bytes */total`, 308 Range), OneDrive `nextExpectedRanges`; Go restarted the chunk.
- Delete/download never create folders; a missing item or folder counts as deleted. Manifest delete joins errors across parts and still deletes the manifest.
- Compact manifest JSON; part and whole-file hashes in separate passes.
- Invalid grants are credential errors (non retryable). An unreadable or missing `tokenExpiresAt` forces an immediate refresh (Go treated zero as never expiring).
- Session and pre-authenticated URLs never receive a bearer token and never appear in errors. Refresh requests send no `scope` (the grant's scopes apply).

## Refresh-token rotation decision
A rotated refresh token is kept in memory for the agent's lifetime only and is not reported to the hub, identical to Go. Persisting it would need a hub endpoint that does not exist; if a provider rotates and the agent restarts, the hub's stored token is used again. Microsoft and Google normally keep refresh tokens valid, so this matches current behaviour.

## Gaps
- No live Google/Microsoft call was made; behaviour is verified against hand-written fakes on a local socket (modelled on Go's tests and the documented API shapes).
- OneDrive authority is always `common`, as in Go.

## Tests
`cargo test -p dl-agent-backup -p dl-agent-bin`: 221 passed in dl-agent-backup (new: gdrive 13, onedrive 14, oauth 2, mod/report additions), 0 failed. Covered: refresh, expired token one refresh, 401 retry once without loop, chunked session with an interrupted chunk resumed, 429 + `Retry-After` then give up, downloads (OneDrive redirect has no Authorization), delete of missing item, cancel between chunks, manifest checksum mismatch, a failing part still deleting the manifest, Go path parse tests. The pipeline test for an unsupported provider now uses `dropbox`.
`cargo clippy -p dl-agent-backup -p dl-agent-bin --all-targets -- -D warnings`: clean on host and `--target x86_64-pc-windows-gnu`. `cargo fmt --all` applied. All files under 800 lines (largest onedrive.rs ~700).

## Trailer note
The brief names Opus 5.5 for Co-Authored-By; the session instruction says Sonnet 5.5. The commit uses the session's Sonnet 5.5 trailer, the model that did the work.
