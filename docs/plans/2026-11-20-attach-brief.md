# Brief: attach another product to an enrolled dl-agent (RT-0298, urgent)

## Goal
One customer = one dl-agent. A machine first enrolled for one product (e.g. Check-In / `arrival`)
must be able to take a second product (e.g. PDAOrder / `pda-sync`) WITHOUT losing its identity,
its config, or the first product's module. First customer: Muine Bay Resort (Check-In RT-0252 + PDAOrder RT-0225).

Today: `cli_install_win.rs` enroll_step prints "Already enrolled: keeping this machine's identity"
and ignores the new code, so the new module stays disabled; `-ReEnroll` wipes the identity.
Product codes (`services/agent-hub/src/services/productKeys.ts` mintProductCode) create tenant
`<product>-<external_id>` and carry config `{modules:{<product>:{enabled:true}}}`.

## Scope (one slice)
### Hub (services/agent-hub)
1. Migration `0008_product_links.sql`: table `product_links(product TEXT, external_id TEXT,
   tenant_id TEXT, agent_id TEXT, site_id TEXT NULL, created_at INTEGER, PRIMARY KEY(product, external_id))`
   + index on agent_id. Backfill rows for existing agents enrolled with a product-minted code
   (enroll_codes.minted_by not null) if that join is available; otherwise skip backfill and say so.
2. Signed agent endpoint `POST /v1/attach` body `{code}` (same signature scheme as heartbeat/reports;
   reuse the existing signed-route middleware). Behaviour:
   - code must exist, be unused, unexpired, not revoked (same checks as enroll; reuse helpers).
   - deep-merge `code.config.modules` into the agent's current config (`modules.<x>` sections from the code
     win only for keys they set; other modules untouched); keep other top-level keys of the current config;
     bump config version exactly like `setConfig` does so the agent picks it up via last-known-good.
   - if the code was product-minted: upsert product_links(product, external_id derived from the code's
     tenant `<product>-<external_id>`, tenant_id = the AGENT's tenant, agent_id, site_id = agent's site).
   - mark the code used (used_by = agent id, used_at) atomically (D1 batch / conditional UPDATE so two
     attaches with one code can't both win).
   - Idempotent: attaching the same product twice just re-merges; return 200 with `{config, attached:[products]}`.
   - Audit row like other admin/agent writes if an audit table exists.
3. `mintProductCode`: if product_links already has (product, external_id) → the code still mints normally
   (agent attach will just refresh). No other behavior change.
4. Admin: `GET /admin/agents/:id` (or the existing agent view) includes the agent's product links.
5. Tests (vitest, existing helpers in test/): attach happy path keeps prior module + adds new one;
   used/expired code rejected; unsigned/wrong-signature rejected; double attach race → one wins;
   product link recorded; idempotency.

### Agent (crates/dl-agent)
6. `HubClient` gains `attach(code)` (signed POST /v1/attach); apply returned config through the same
   path enroll uses (persist + last-known-good).
7. CLI: `dl-agent attach --code CODE` (cli.rs) — requires an existing identity; clear error otherwise.
8. Installer (`cli_install_win.rs` enroll_step): if already enrolled AND a code is given AND not `-ReEnroll`
   → call attach instead of ignoring the code; print "Attached <products> to this agent". The following
   `-Setup <tool>` step runs as today. Not enrolled → enroll as today. Unit tests where the module has them.
9. `cargo test -p dl-agent` and `cargo clippy -p dl-agent` clean for touched code (Windows-only files:
   at least `cargo check` for the host target; note any cfg(windows) code you could not compile).

### Docs
10. `crates/dl-agent/PROTOCOL.md` §2: document `/v1/attach`. README/install doc: "same install line works
    for a second product". Decision record `docs/decisions/0002-one-agent-many-products.md`
    (context, decision, consequences; later work: remote attach from admin/product, product key may
    only write its own modules.<product>, tenant merge for already-split tenants, detach).
11. CHANGELOG entry if the repo has one.

## Out of scope
Deploying the hub, releasing a binary, remote/admin attach, detach, tenant merge.

## Rules
English code/commits, Conventional Commits, end commit message with
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Do not push/merge/deploy.
Run only touched tests: `cd services/agent-hub && pnpm -s typecheck && pnpm -s vitest run <files>`; heavy cargo via
`~/.claude/scripts/heavy.sh`. Files ≤ 800 lines. No empty catches.

## Report
Write `docs/plans/2026-11-20-attach-report.md`: what changed, commit SHA, tests run + result, gaps.
