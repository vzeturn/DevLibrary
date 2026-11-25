# Brief B2b: Google Drive and OneDrive uploaders for `dl-agent-backup` (RT-0164 task 557, step 4)

## Goal

`crates/dl-agent-backup` (merged in PR #30, read its README, `src/upload/mod.rs` and the B2a
report `docs/plans/2026-11-19-backup-module-report.md`) has local, S3/R2 and Azure uploaders. A
schedule targeting Google Drive or OneDrive fails today with `backup_destination_unsupported`.
Port the two remaining uploaders from the Go agent so the module covers every destination the
SRBackup hub offers.

Reference (read only, do not modify): `~/Projects/sr-backup/agent/internal/uploader/up_gdrive.go`
(387 lines) and `up_onedrive.go` (722 lines) plus their tests, and how `hub/src/routes/agent.ts`
sends the destination (fields, `credentialError`). The SRBackup main checkout has another
session's uncommitted work (a hub-side "managed Drive"): read only, never write there, and do not
build anything for managed Drive in this slice; just keep the uploader factory easy to extend with
another destination kind.

## Port

- **Google Drive**: OAuth refresh (`clientId`, `clientSecret`, `refreshToken` → access token,
  cached until shortly before expiry, refreshed once on 401), folder lookup/creation as Go does,
  resumable upload in chunks (multiple of 256 KiB) with resume after a dropped connection,
  download, delete (already gone = deleted), the cloud path format Go stores (keep it identical,
  the hub and restores depend on it).
- **OneDrive / Microsoft Graph**: refresh token flow (tenant/authority as Go uses it), upload
  session for large files (chunk sizes multiple of 320 KiB, as Graph requires), small-file simple
  upload if Go has one, folder creation, download (follow the redirect to the pre-authenticated
  URL **without** sending the bearer token to it), delete, same cloud path format as Go.
- A rotated refresh token returned by the provider: do what Go does (store/report or ignore);
  say which in the report.
- Both honour `CancelToken` between chunks and report progress. HTTP via `dl-http`/`dl-tls`
  (repo dependency policy, no new third-party crates). Throttling (429/503 + `Retry-After`) →
  bounded retry, then `backup_upload_failed`.
- Remove the `backup_destination_unsupported` path for these two kinds (keep the code for
  unknown kinds).
- Never log or report tokens, client secrets, upload-session URLs (they are bearer
  capabilities) or file contents; extend the existing scrubber if needed.

## Tests (mirror source structure, hand-written fakes, no mocking crates)

- Port the Go uploader tests for both providers.
- Fake provider on a local socket for each: token refresh, expired token → one refresh + retry,
  resumable/session upload across several chunks with one interrupted chunk resumed, 429 with
  `Retry-After`, download (OneDrive redirect must not carry the Authorization header), delete of a
  missing item, cancel between chunks.
- The provider base URLs must be overridable only from tests (not from hub config).
- No live Google/Microsoft calls in tests.

## Checks before commit

- `~/.cargo/bin/cargo fmt --all`
- `~/.cargo/bin/cargo test -p dl-agent-backup -p dl-agent-bin`
- `~/.cargo/bin/cargo clippy -p dl-agent-backup -p dl-agent-bin --all-targets -- -D warnings`,
  and the same with `--target x86_64-pc-windows-gnu`.
- Every file ≤ 800 lines (200–400 typical). No swallowed errors. Heavy builds via
  `~/.claude/scripts/heavy.sh` (if the shared lock is held by another session's job, use a private
  `HEAVY_LOCK=/tmp/heavy-dl-bk2`, never kill someone else's job).

## Rules

- Work only in worktree `~/Projects/DevLibrary-bk2`, branch `feat/dl-agent-backup-oauth`.
  Conventional Commits in English, trailer:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k`.
  **Do not push, merge or deploy.** Do not modify SRBackup. Never print secrets.
- Report to `docs/plans/2026-11-19-backup-oauth-report.md`: what was ported, differences from Go
  and why, the refresh-token rotation decision, gaps (no live provider test), test counts.
