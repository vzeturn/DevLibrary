# Report: attach another product to an enrolled dl-agent (RT-0298)

- Brief: `docs/plans/2026-11-20-attach-brief.md`
- Branch: `feat/rt-0298-attach` (worktree `/Users/imac/Projects/DevLibrary-wt-rt0298`)
- Commit: `bac9cab` feat(agent): attach another product to an enrolled agent (RT-0298). Not pushed.
- Status: DONE_WITH_CONCERNS (see Gaps)

## What changed

### Hub (`services/agent-hub`)

| # | Change | Files |
|---|---|---|
| 1 | Migration `0008_product_links.sql`: `product_links(product, external_id, tenant_id, agent_id → agents, site_id NULL, created_at, PK(product, external_id))` plus the index `product_links_agent`. Backfill from `enroll_codes.minted_by` joined to `agents` through `used_by_agent`. The external id is the code's tenant without the `<product>-` prefix. When several agents used codes of one product tenant, the newest live agent wins. | `migrations/0008_product_links.sql` |
| 2 | Signed `POST /v1/attach {code}` (behind `requireSignedAgent`, the same middleware as heartbeat and reports). Details below this table. | `src/routes/agent.ts`, `src/services/attach.ts`, `src/repo/productLinks.ts`, `src/repo/enrollCodes.ts` (`findCodeById`), `src/domain/schemas.ts` (`attachBody`) |
| 2b | Enrolment with a product-minted code also records the product link, in the same batch as the claim. Without this the admin view would only cover backfilled and attached agents. | `src/repo/enrollCodes.ts` (`claimCodeAndCreateAgent`) |
| 3 | `mintProductCode` is unchanged. It still mints when a link exists; I only added a doc comment. A test proves a second mint for a linked product tenant works and that attaching that code refreshes the link without a new config version. | `src/services/productKeys.ts` |
| 4 | Admin: new `GET /admin/agents/:id`, which answers 404 `agent_not_found` for an unknown id. `GET /admin/agents` now gives each agent a `products: [{product, external_id, created_at}]` list. | `src/services/admin.ts`, `src/routes/admin.ts`, `src/repo/agents.ts` |
| 5 | Vitest: 14 tests. Cases are listed under Tests. | `test/attach.test.ts` |

How `POST /v1/attach` behaves:

- **Code checks.** It uses the same checks as an enrolment (`sha256(normaliseCode(code))`; unused, not revoked, not expired). Otherwise it answers `403 enroll_code_invalid`.
- **Config merge.** The code's `config.modules` is deep-merged into the agent's current config, with `DEFAULT_CONFIG` used when the agent has none. Each `modules.<x>` key in the code wins. Other modules and other top-level keys stay. `__proto__` keys are skipped.
- **Version bump.** It works like `putConfig` (`version + 1`), and only happens when the merge actually changed the body.
- **One batch.** The claim, the config upsert and the link upsert run in one D1 batch:
  - The claim is a conditional `UPDATE`: the code must be unused, live and not expired, and the agent's config version must still be the one that was read.
  - The config and link writes only land while this agent holds the code.
  - The config write is also guarded by the version.
  - If anything moved in between, the code and config are re-read, up to 3 attempts; after that it answers `409 attach_conflict`.
- **Race.** Two attaches with one code have one winner (403 for the other). A config an admin changed in between is re-read and never overwritten.
- **Idempotency.** A code this agent already used (its enrolment code, or an earlier attach) re-merges and answers 200. There is no new version when nothing changed. So running a product's install line again is harmless.
- **Product link.** For a product-minted code, the link is upserted: `(product, external_id)` → the agent's own tenant and site.
- **Answer.** `200 {config: {version, body}, attached: [module names]}`.
- **Code without modules.** It is refused with `400 code_has_no_modules`, and the code stays unused.
- **Audit.** An `admin_audit` row is written before the change, with actor `agent:<id>`, path `/v1/attach` and body `{code_id, minted_by, attached}` (never the code). The status or error is written after. It fails closed with `503 audit_unavailable`, like the admin audit.

### Agent (`crates/dl-agent`)

| # | Change | Files |
|---|---|---|
| 6 | Signed POST `HubClient::attach(code) -> Attached {config, attached}` and `ATTACH_PATH`. A config is returned only when its version is above 0. | `src/hub.rs`, tests in `src/hub_tests.rs` |
| 7 | `dl-agent attach --code CODE` (or env `DL_AGENT_ENROLL_CODE`; `--data`). It loads the identity; without one it fails with `agent is not enrolled` plus a `Next:` line, and nothing is sent. It applies the answered config through `ConfigStore::apply`, the same path enrolment uses (persisted, last-known-good kept, trial). It prints `Attached pda-sync to this agent (config version N)`. Each failure maps to a next step (used/expired code, older hub with 404, revoked agent, code without modules). | `src/cli_attach.rs` (new), `src/cli.rs` (grammar and dispatch arm), `src/lib.rs` |
| 8 | Installer `enroll_step`, for an enrolled machine with a code and without `-Reenroll`: it calls attach and prints `Already enrolled: keeping this machine's identity` followed by `Attached <modules> to this agent ...`. Details below this table. | `src/cli_install_win.rs`, `src/cli_attach.rs` (`install_step`) |

How the installer step handles each case:

- **No code.** Same as before: "Already enrolled".
- **Plain code without modules** (`code_has_no_modules`). It keeps the previous behaviour ("Already enrolled ... (the code adds no product)"), exits 0, and the code stays unused.
- **Any other failure.** It prints a warning and returns a deferred `Failure` (exit 1). The setup then still runs `-Setup`, replace, target and service registration, and returns the failure at the end, after the existing `sql.failed` check. The reason: the setup has already stopped the agent's own service and unregistered its watchdog (`remove_old_copies`), so failing early would leave the first product down.
- **Not enrolled, or `-Reenroll`.** Enrolment runs as before.

### Docs

| # | Change | Files |
|---|---|---|
| 10 | PROTOCOL §2: a row in the endpoint table plus a new §2.3 "Attach: one agent, many products". The agent README: "The same install line works for a second product", plus `dl-agent attach`. The hub README: the endpoint, the admin agent view, and an installer note. Decision record 0002, covering context, decision, consequences and later work: remote attach, product key limited to its own `modules.<product>`, tenant merge, detach, and the legacy installer. | `crates/dl-agent/PROTOCOL.md`, `crates/dl-agent/README.md`, `services/agent-hub/README.md`, `docs/decisions/0002-one-agent-many-products.md` |
| 11 | CHANGELOG `[Unreleased] / Added` entry. | `CHANGELOG.md` |

## Tests and gates (all run once, all green)

| Command | Result |
|---|---|
| `cd services/agent-hub && pnpm -s typecheck` | clean (src + test) |
| `cd services/agent-hub && pnpm -s vitest run test/attach.test.ts test/productKeys.test.ts test/enroll.test.ts test/heartbeat.test.ts test/adminAudit.test.ts test/signature.test.ts test/hubUrls.test.ts test/releases.test.ts test/demo.test.ts` | 9 files, 98 tests passed. The new file and the direct callers of enrol, product codes, the admin agent list (heartbeat, hubUrls, releases), audit and signature all pass. The `admin_audit_write_failed` log line comes from `adminAudit.test.ts` dropping the table on purpose. |
| `~/.claude/scripts/heavy.sh cargo test -p dl-agent` | lib 247 passed, 2 ignored (existing); other targets 4 + 1 + 2 passed (1 ignored) |
| `~/.claude/scripts/heavy.sh cargo test -p dl-agent --lib attach` | 6 new tests passed (2 in `hub_tests`, 4 in `cli_attach`) |
| `~/.claude/scripts/heavy.sh cargo clippy -p dl-agent --all-targets` | no warnings |
| `~/.claude/scripts/heavy.sh cargo check -p dl-agent --target x86_64-pc-windows-gnu` | clean. `cli_install_win.rs` compiles on every non-wasm target (runtime `cfg!(windows)`), and also for Windows. |
| `cargo fmt -p dl-agent --check` | clean |

`test/attach.test.ts` covers:

- **Happy path.** An agent enrolled for `arrival` attaches a `pda-sync` code. Its `arrival` settings and other top-level keys are kept, the version goes from 2 to 3, the tenant stays, and the next heartbeat carries the merged config.
- **Product links and admin views.** Both links are recorded. `GET /admin/agents/:id` returns products (and 404 for an unknown id), and `GET /admin/agents` returns products.
- **Bookkeeping.** The code is marked used, and the audit row has actor `agent:<id>` and no code.
- **Idempotency.** Attaching the same code again gives the same answer and no bump. Re-running the first product's enrolment code gives 200 and no bump. A second mint for a linked product tenant works, and attaching it changes nothing but the code.
- **Module re-enabled.** A module disabled by an admin is enabled again by its code, and the admin's other keys are kept.
- **Refused codes.** Codes used by another agent, expired, revoked, unknown or too short are refused, and the config stays at version 1.
- **Admin codes.** An admin code without modules gets 400 and stays unused. An admin code with modules merges only `modules`, with no product link.
- **Signatures and revocation.** Unsigned, wrong-key, tampered-body and revoked agents get 401, and the code stays unused.
- **Races.** Two agents racing for one code end as 200 and 403: the winner has version 2, the loser 1, and the link points to the winner. The same agent sending one code twice at once gets a single bump. An admin config write during an attach is not lost.
- **Enrolment link.** Enrolling with a product code records the link.
- **Backfill.** The SQL is replayed against real enrolments: the newest live agent wins and admin codes are skipped.
- **Merge rules.** `mergeModules` and `deepMerge` unit tests, including `__proto__`.

## Gaps and concerns

1. **The hub is not deployed** (out of scope). Until migration 0008 and the route are live, an attach from the new binary answers 404. The installer then ends with exit 1 and the next step "the hub does not know attach yet". Before this change the same run exited 0 and silently left the module off. So: **deploy the hub before releasing this agent build** to the Muine Bay Resort machine.
2. **Behaviour change in the installer.** Re-running an install line on an enrolled machine with a code that is expired, revoked, or used by another machine now ends with exit 1 after the service is back (it used to exit 0, "Already enrolled"). This is intended: the code no longer gets silently ignored. A plain admin code without modules still exits 0, and a code this agent already used gets 200.
3. **The Windows 7 / PowerShell 2.0 legacy installer is unchanged** (`src/install/legacyScript.ts`, around line 379). It still only says "Already enrolled". On such a machine, run `dl-agent attach --code ...` by hand. This is listed as later work in decision 0002; I did not widen the slice.
4. **No admin page in the hub UI shows the links.** They are in the JSON API (`products` on agents) only.
5. **No product-key scope on attach yet.** The hub does not check that a product code only carries its own `modules.<product>`. Today `mintProductCode` only builds that one section, so the risk is theoretical. This is listed as later work.
6. **The second product's tenant is not used.** An attached agent stays in its first tenant, and product platforms must look up their agent through `product_links`, not through the tenant `<product>-<external_id>`. The PDAOrder side (RT-0225) may need to read the link, or the hub API, to find the agent. That was not checked here.
7. **Uncommitted files.** The brief and this report are left uncommitted in `docs/plans/` for the main session to decide.
