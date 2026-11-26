# 0011. Realtime store <-> cloud: LAN-first edge, event-driven capture, one duplex session

- Status: proposed (design + spike; nothing in production changes with this ADR)
- Date: 2026-11-26
- Scope: `crates/dl-agent-pda-sync` (capture, outbox, transport, commands), the PMS module,
  the PDAOrder cloud (.NET 10 on Fly `sin`) and the azolo-pms cloud; RT-1318
- Spike: `spikes/dl-rt-transport` (standalone, not a workspace member; see its README)

## Context

Owner goal (RT-1318): a SMILE SQL change on site is applied in the cloud in p50 <= 40 ms and
p95 <= 70 ms on a healthy link; a cloud command reaches the store and its result is back in
p95 <= 100 ms. The design must serve PDAOrder and the PMS front desk alike.

Today (dl-agent 1.3.51):

| Stage | Today |
|---|---|
| Capture | SELECT-only polling of `SMILE_POS` as `pda_sync`, 2 s cycle, two-phase signatures (1.3.53 fixes the loop to < 1 s in another worktree) |
| Upload | outbox, one signed HTTP POST per batch, one batch every 4-6 s; ingest POST 40-90 ms |
| Commands | WebSocket `/sync/v1/commands/ws`, results posted back |
| Path | agent -> Cloudflare -> Fly `sin` (Kestrel, Postgres) |

So a store change reaches the cloud in seconds, and every staff action on a PDA crosses the
Internet twice even though the PDA, the KDS, the printers and the agent share one hotel LAN.

Physics sets the floor: a store in Vietnam is about 30-45 ms RTT from Singapore, so one way is
15-25 ms. A 40 ms p50 leaves about 15 ms for detection, reading, framing and the cloud commit
together. Neither a per-batch HTTP request nor a poll cycle of hundreds of milliseconds fits.

## Decision

### D1. LAN-first edge (approved direction, owner 2026-11-26)

Staff devices talk to the agent on the hotel LAN when they can, and to the cloud otherwise.
The cloud stays the system people log in to and the replica everything else reads.

1. **Same domain, discovered endpoint.** The app keeps loading from Cloudflare. After login
   the cloud returns the site's LAN endpoint `https://<site>.lan.<domain>` (one name per
   site). The cloud owns that public DNS A record and points it at the agent's LAN IP; the
   agent reports IP changes over the session (D3), and the cloud updates the record. The app
   probes `/edge/v1/health` with a 200 ms budget; on success it uses the edge for API and
   push, else it uses the cloud with no visible difference. It re-probes on network change
   and every 60 s while on the cloud path.
   Known limit: some routers and resolvers drop public answers that point at private IPs
   (DNS rebinding protection). Then the probe fails and the app stays on the cloud: correct,
   only slower. The probe result is reported per site so ops see it.
2. **Real TLS per site.** The agent generates its key pair locally and sends a CSR for
   exactly `<site>.lan.<domain>`. The cloud runs ACME DNS-01 (the DNS API token lives only
   in the cloud, never on site), returns the chain over the session, renews at one third of
   remaining life, and revokes on unenrol. Lifetime: the CA's short-lived profile where
   offered (Let's Encrypt `shortlived`, 160 h), else the 90-day default. The private key never
   leaves the agent; the certificate covers only that site's name.
3. **Edge security.**
   - The edge accepts only the staff session tokens the cloud issues: signatures verified
     offline with the cloud's public keys (pushed over the session, rotated by `kid`), short
     TTL, and a revocation list pushed within seconds. The same RBAC and tenant checks as the
     cloud run from one shared rule table, so the two paths cannot drift.
   - No admin or ops endpoints on the LAN: staff API and push only. Admin stays cloud-only.
   - Rate limits per token and per source IP; request bodies capped.
   - Audit log of every edge write, kept in `outbox.db` and synced to the cloud.
   - Browser rules: CORS allows only our app origin. Preflights carry
     `Access-Control-Allow-Private-Network: true` only for that origin; the app's LAN fetches
     carry `targetAddressSpace: "local"` for Chrome's Local Network Access (permission prompt
     from Chrome 142, secure contexts only).
   - DNS rebinding: the edge answers only when `Host` is exactly `<site>.lan.<domain>`
     (else 421), on top of the TLS name check. It binds to the LAN interface only; the
     Windows firewall rule it installs is scoped to the local subnet.
   - No customer data leaves the site except through the existing sync. Agent updates stay
     signed (ADR 0001, 0009).
4. **Consistency.**
   - `txn_authority=site`: SMILE on site is the single writer for site-owned entities
     (checks, items, folios). The cloud is a replica plus a command relay; it never writes
     its replica of site-owned rows directly.
   - Every write carries a client idempotency key (UUIDv7) end to end. The agent keeps key ->
     result for 24 h in local SQLite; a write sent on the LAN and retried through the cloud
     relay returns the first result. The cloud dedupes the same key too.
   - Conflicts: both paths end in SMILE in one serial order. A write carries the row
     version the user saw (the replica's version or the edge's); on mismatch the agent
     answers `conflict` and the app reloads that object. No silent last-writer-wins.
   - Cloud down: the edge keeps serving the LAN; the audit log and capture queue in
     `outbox.db`. Agent down: the app falls back to the cloud, and cloud commands for the
     site wait or expire on their lease, exactly as today.
5. **Rollout.** A per-site flag (default off) and a global kill switch, both runtime settings
   in the hub (no release needed). Canary at MuineBay with automatic fallback to the cloud
   path, per-hop metrics from day one, then the PDAOrder sites, then the PMS front desk.

### D2. Event-driven capture: Change Tracking first, fast polling as fallback

| Option | SQL 2008R2-2019 Express | Grants | Touches SMILE schema | Latency | Verdict |
|---|---|---|---|---|---|
| **Change Tracking (CT)** | yes, every edition | enable once (`ALTER DATABASE`, `ALTER TABLE`); read with `VIEW CHANGE TRACKING` + SELECT | no (internal side tables) | poll of a version number, ~10 ms average at a 20 ms cycle | **chosen** |
| Triggers + queue table | yes | ALTER on SMILE tables | yes: triggers run inside SMILE's transactions | ~ms | rejected: a slow or failing trigger breaks POS writes in a vendor schema |
| CDC | no: needs SQL Server Agent (Express has none) and sysadmin | high | change tables | log reader, seconds | rejected |
| Query notifications / Service Broker | yes, but `ENABLE_BROKER` takes an exclusive DB lock and `dl-tds` lacks the TDS notification header | extra | no | push | later, only if CT polling cost shows up |
| Signature polling (today) | yes | SELECT | no | 250 ms - 2 s | fallback |

- CT is enabled per captured table that has a primary key (CT requires one), by the same
  bootstrap identities as ADR 0010 (service Windows identity, then the SMILE login; never
  altering that login). Retention 2 days, auto cleanup on.
- The reader polls `CHANGE_TRACKING_CURRENT_VERSION()` every 20 ms (one tiny query); when it
  moves it reads `CHANGETABLE(CHANGES t, @last)` joined to the table, and stores the new
  version in `capture.db` only after the batch is committed to `outbox.db`, so a crash re-reads.
  If `CHANGE_TRACKING_MIN_VALID_VERSION` passed our version, that table falls back to the
  existing full resync (shadow manifest and key repair).
- Tables without a primary key, or a site where enabling CT is refused, keep signature
  polling at the 1.3.53 cadence. The mode is chosen per table and reported (`CAPTURE_MODE`).
- CT adds one side-table row per DML on tracked tables. SMILE POS write rates are tens per
  minute per store; the canary measures commit time before and after.

### D3. One long-lived duplex session per agent, WebSocket first

One session per agent (`dl-rt/1`) carries everything: capture batches up, commands down,
results up, heartbeats, config and certificate pushes. It replaces per-batch POSTs and folds
`/sync/v1/commands/ws` in (kept as fallback during rollout).

- **Frames:** `chan | kind | seq | sent_us | payload` (the spike's format). Channels:
  control, capture, commands, results, bulk (blobs, resync).
- **Seq / ack:** per channel monotonic `seq`. The cloud acks cumulatively after the durable
  commit. The agent keeps unacked frames in `outbox.db` (as today).
- **Resume:** on reconnect the agent sends `resume{chan, last_acked}`; the cloud answers with
  its high-water mark; the agent resends from there. Apply is idempotent by
  `(agent_id, chan, seq)` and row keys, so a duplicate is harmless.
- **Backpressure:** credit-based. The cloud grants bytes per channel. Without credit the agent
  stops sending and the outbox absorbs the backlog. Bulk never takes the control/capture
  credit, and the writer prefers control/capture frames. Bulk (resync, blobs) runs on a
  second WebSocket connection of the same session, so a lost bulk segment never blocks
  capture or commands behind it (TCP head-of-line; see D5).
- **Auth:** the upgrade request is signed with the agent's Ed25519 key (`MM-SYNC1`, as the
  command socket is today) inside TLS; the session binds to that agent id. No new secret,
  nothing in the repo. mTLS can come later; it does not change the protocol.
- **Transport: WebSocket over TLS on the existing path** (agent -> Cloudflare -> Fly), via
  `dl-ws` + `dl-tls` (SChannel on Windows) which the agent already ships. Reasons:
  - Fly: UDP needs a dedicated IPv4 and `fly-global-services` binding, has no per-flow load
    balancing (all packets near a region go to one Machine, and when that Machine changes
    every flow moves at once), and "QUIC isn't explicitly supported" ("expect breakage").
  - Cloudflare does not carry HTTP/3 or QUIC to the origin; raw UDP needs Spectrum
    (Enterprise, paid add-on). QUIC would bypass Cloudflare and its WAF/DDoS shield.
  - .NET QUIC needs `libmsquic` installed in the Linux image.
  - DevLibrary allows no third-party crates; quinn/rustls would need an exception ADR, and
    writing QUIC ourselves is out of proportion.
  - The spike (D5) shows WebSocket and QUIC equal on a healthy link once the session is
    warm. QUIC wins at connect time and when loss meets a busy session.
  QUIC stays an option behind the same frame protocol (D6) if per-hop metrics show that loss
  tails break the p95 at real sites.

### D4. Streaming apply and push in the cloud

- The Kestrel session endpoint decodes frames into an in-process `Channel<T>` per tenant. One
  applier per tenant group-commits whatever arrived within 2 ms or 256 frames: one Postgres
  transaction (multi-row upsert, reusing the ingest apply code), one commit with
  `synchronous_commit` on, then a cumulative ack. No HTTP request per batch.
- After commit the applier publishes `NOTIFY` (payload = tenant + entity keys). Every Machine
  listens and fans out to clients over SSE (or the existing WS), so the session and the
  client may sit on different Machines.
- Commands use the same session: cloud enqueue -> push on the commands channel -> agent runs
  them against SMILE -> result frame -> persist -> notify the waiting client.

### Latency budget per hop

Assumes 35 ms RTT store <-> `sin`, a warm session, and CT capture. Numbers in ms.

| Hop (store change -> cloud) | Today | Target p50 | Target p95 |
|---|---|---|---|
| SMILE commit -> change detected | 0-2000 (poll) | 10 | 20 |
| Read changed rows, frame the batch | tens to hundreds | 3 | 8 |
| `outbox.db` append (in parallel with the send, not on the path) | - | 0 | 0 |
| Agent -> Cloudflare -> Fly, one way | POST 40-90 plus a 4-6 s batch wait | 18 | 30 |
| Decode + group commit in Postgres | inside the POST | 4 | 10 |
| **Total** | **seconds** | **35** | **68** |

| Hop (cloud command -> store -> result) | Target p50 | Target p95 |
|---|---|---|
| Enqueue + push on the warm session, one way | 18 | 30 |
| Agent runs it on SMILE | 10 | 30 |
| Result, one way | 18 | 30 |
| Persist + notify the client | 3 | 8 |
| **Total** | **49** | **98** |

| LAN edge (PDA -> agent -> SMILE -> PDA) | Target p50 | Target p95 |
|---|---|---|
| Wi-Fi + TLS on the LAN, both ways | 3 | 10 |
| SMILE write | 10 | 30 |
| **Total** (cloud learns via capture ~35 ms later) | **13** | **40** |

Each hop is measured: frames carry `t_detected`, `t_read`, `t_sent`; the cloud stamps
`t_recv`, `t_committed`; the edge stamps its own. Per-site histograms are kept in the hub. The
p95 target is judged on those numbers, not on the budget above.

### D5. Spike results (`spikes/dl-rt-transport`)

Measured 2026-11-26 on the Intel iMac (macOS, loopback), `cargo run --release --bin rt-bench`;
full output in the spike README. 1 KiB ping = one small capture batch or command and its ack.
Times are round trips in ms (p50 / p95 / p99); one way is about half.

| transport | link | connect p50 | RTT idle | RTT under bulk (4 x 16 KiB in flight) | bulk MB/s (64 in flight) |
|---|---|---|---|---|---|
| quic | loopback | 0.6 | 0.1 / 0.2 / 0.2 | 0.8 / 1.7 / 2.3 | 109.4 |
| ws+tls | loopback | 1.1 | 0.1 / 0.2 / 0.2 | 0.2 / 0.3 / 0.3 | 559.6 |
| quic | 30ms RTT, 50 Mbit/s | 34.1 | 33.2 / 34.6 / 35.1 | 34.7 / 36.2 / 36.9 | 5.6 |
| ws+tls | 30ms RTT, 50 Mbit/s | 66.7 | 33.1 / 34.2 / 34.7 | 35.3 / 36.6 / 37.1 | 6.1 |
| quic | 30ms RTT, 50 Mbit/s, 1% loss | 33.8 | 33.2 / 34.5 / 126.9 | 42.0 / 65.5 / 88.3 | 0.5 |
| ws+tls | 30ms RTT, 50 Mbit/s, 1% loss | 66.5 | 33.4 / 34.7 / 93.4 | 35.4 / 96.2 / 98.2 | 5.9 (TCP loss modelled, no congestion control) |

Reading:

1. **Healthy link, sparse traffic (the capture and command path):** QUIC and WebSocket are
   the same, 33 ms p50 and 34-35 ms p95 at 30 ms RTT, about 3 ms over the configured RTT.
   One way is ~17 ms, which fits the 18 / 30 ms budget hop. The transport is not what keeps
   us from the target; polling and per-batch HTTP are.
2. **Loss with a busy session:** QUIC keeps control frames off a lost bulk packet's path
   (p95 65.5 vs 96.2 ms). Plain WebSocket gets the same effect by carrying bulk (resync,
   blobs) on a **second** WebSocket connection, so a bulk loss never blocks capture or
   commands (D3).
3. **Idle loss tail:** the p99 is set by the loss timer on both: quinn's probe timeout
   (126.9 ms) vs the 2 x RTT TCP model (93.4 ms). Real TCP is between a tail-loss probe
   (~2 RTT) and a minimum RTO of 200-300 ms. At 1% loss this touches the p99, not the p95.
4. **Connect:** QUIC is 1 RTT; WebSocket shows 2 RTT here, and 3 in reality (the shim does
   not delay the TCP handshake). Only reconnects pay this, since the session is long-lived.
5. **Throughput** is not a goal here. QUIC under 1% random loss drops to 0.5 MB/s (its
   congestion control at work); the TCP figure under loss does not model congestion control.
   Loopback figures are CPU-bound and not comparable.

### D6. Alternatives compared

Expected latencies are store change -> cloud commit with the same capture (D2) and a 35 ms RTT;
"effort" is in one-day slices.

| Option | p50 / p95 expected | Offline behaviour | Ops cost | .NET + Rust maturity | Effort | Verdict |
|---|---|---|---|---|---|---|
| **LAN-first edge** (D1) | staff path 13 / 40 on the LAN; cloud copy as below | LAN keeps working with the cloud down | per-site DNS + ACME in the cloud; no new servers | new code: `dl-tls` and `dl-http` are client-only today, so the edge needs SChannel server mode (`AcceptSecurityContext`) and a small HTTP/1.1 + WS server; ASP.NET for the cloud parts | ~11 slices | **chosen** |
| **Own session over WebSocket** (D3) | 35 / 65 | outbox buffers; resume by seq | none new | `dl-ws` (ours), ASP.NET Core WebSockets (mature) | ~8 slices | **chosen** |
| Own session over QUIC | 34 / 60 (better tail under loss) | same | dedicated IPv4 UDP on Fly, Cloudflare bypassed, `libmsquic` in the image, one Machine per region receives UDP | quinn mature; `System.Net.Quic` stable since .NET 9; third-party ban in DevLibrary | +5 slices on top of WS | later, if metrics demand |
| NATS JetStream | 38 / 70 (one more hop through the NATS server) | the client does not persist offline publishes: still needs our outbox, or a leaf node on site (one more binary to ship) | 3-node cluster with volumes on Fly, accounts/nkeys per agent, upgrades, monitoring | nats.net v2 and async-nats good; third-party ban on the agent unless we write the client | ~12 slices | rejected: replaces an outbox + ack we already have with infrastructure we would run |
| gRPC bidi streaming (tonic <-> .NET) | 35 / 65 (HTTP/2 over TCP: same head-of-line as WS) | same as WS | Cloudflare proxies gRPC; none new | Grpc.AspNetCore mature; tonic needs hyper/h2/prost (ban) | ~10 slices | rejected: no latency gain over WS, heavier stack |
| WebTransport | as QUIC | same | as QUIC, and Cloudflare cannot carry it to the origin | Kestrel support is experimental; no maintained Rust client we could adopt | L | rejected |
| Zenoh | ~35 / 65 with a router in `sin` | strong on the LAN (peer mode) | Zenoh routers to run | Rust native; .NET only through C bindings, immature | XL | rejected for now |

## Implementation plan (each slice <= 1 day, in dependency order)

| # | Slice | Repo | Depends on |
|---|---|---|---|
| 1 | Per-hop timestamps on capture frames and ingest; per-site latency histograms in the hub | DevLibrary, PDAOrder | - |
| 2 | Frame codec + seq/ack/credit/resume state machine (pure, no I/O) with property tests (`dl-agent-rt`) | DevLibrary | - |
| 3 | Cloud `/sync/v1/session` WS endpoint: signed upgrade, frame decode, dedupe by (agent, chan, seq) | PDAOrder | 2 |
| 4 | Cloud group-commit applier (`Channel<T>`, 2 ms / 256 frames, one tx) + cumulative ack; Testcontainers PG tests | PDAOrder | 3 |
| 5 | Agent session client on `dl-ws`/`dl-tls`: reconnect with jittered backoff, resume from `outbox.db` | DevLibrary | 2, 3 |
| 6 | Outbox sends on capture (no 4-6 s timer), credit backpressure; POST path kept behind a flag | DevLibrary | 4, 5 |
| 7 | Commands + results on the session; `/commands/ws` stays as fallback | both | 5 |
| 8 | CT probe and enable step in the grants bootstrap (per table, idempotent, flag `capture.change_tracking`) | DevLibrary | - |
| 9 | CT reader: 20 ms version loop, `CHANGETABLE` read, min-valid-version fallback; tests on the cloud test SQL | DevLibrary | 8 |
| 10 | Capture mode per table (CT / fast poll) + `CAPTURE_MODE` site event | DevLibrary | 9 |
| 11 | Cloud push: `NOTIFY` after commit, SSE fan-out to clients | PDAOrder | 4 |
| 12a | `dl-tls` server mode (SChannel `AcceptSecurityContext`, OpenSSL on Linux for tests) | DevLibrary | - |
| 12 | Edge server on the agent: HTTP/1.1 + WS on `dl-http`/`dl-ws` primitives, LAN bind, `/edge/v1/health`, Host check, CORS + private-network headers, rate limits | DevLibrary | 12a |
| 13 | Cloud LAN-endpoint registry + DNS A record management; agent reports its LAN IP on the session | PDAOrder, DevLibrary | 5, 12 |
| 14 | ACME DNS-01 in the cloud from an agent CSR; cert push and renewal over the session; revoke on unenrol | PDAOrder, DevLibrary | 13 |
| 15 | Offline token verification on the edge: key push, `kid` rotation, revocation list push, shared RBAC table | both | 5, 12 |
| 16 | Edge read API for PDAs (menu, open checks) from local state | DevLibrary | 15 |
| 17 | Edge write API (order post) through the command executor, idempotency keys + version check | DevLibrary | 16, 7 |
| 18 | Edge push to PDAs (check changes from CT) | DevLibrary | 10, 16 |
| 19 | PDA app: endpoint discovery after login, 200 ms probe, transparent fallback, behind the site flag | PDAOrder | 13, 14, 16 |
| 20 | Edge audit log synced through the session | DevLibrary | 6, 17 |
| 21 | Rollout controls: per-site flag, kill switch, dashboards; MuineBay canary | hub, PDAOrder | 1, 19 |
| 22 | PMS front desk reuse of the edge and session (azolo-pms) | azolo-pms | 21 |

## Consequences

- Store -> cloud drops from seconds to tens of milliseconds; staff actions on the LAN stop
  depending on the Internet.
- The agent becomes a LAN server: a new attack surface, handled by D1.3, and every edge
  change gets a security review before release.
- CT changes database settings on customer SMILE databases (not the schema of SMILE
  tables); it is reversible (`DISABLE CHANGE_TRACKING`) and runs only where the bootstrap
  identity has the right.
- QUIC, NATS, gRPC, WebTransport and Zenoh are not adopted; the frame protocol stays
  transport-neutral so QUIC can be added later.

## Sources (checked 2026-11-26)

- Fly UDP/QUIC: https://fly.io/docs/networking/udp-and-tcp/ ("You'll need a dedicated IPv4 address
  for your app to accept UDP packets", "We don't support UDP over public IPv6", bind to
  `fly-global-services`, "UDP services don't do per-flow load balancing", "QUIC isn't explicitly
  supported through Fly.io UDP services right now ... you should expect breakage", WireGuard
  takes 60 bytes per packet) and https://fly.io/docs/networking/services/ ("There's currently no
  HTTP/3 or QUIC support").
- .NET QUIC: https://learn.microsoft.com/dotnet/fundamentals/networking/quic/quic-overview
  (stable from .NET 9; "for Linux, you must manually install `libmsquic`"; .NET 7+ needs
  libmsquic 2.2+), Kestrel HTTP/3:
  https://learn.microsoft.com/aspnet/core/fundamentals/servers/kestrel/http3 (Linux needs
  `libmsquic`).
- Cloudflare: https://developers.cloudflare.com/speed/optimization/protocol/http3/ ("HTTP/3
  connection to the origin is not yet supported"); https://developers.cloudflare.com/spectrum/
  ("Custom TCP/UDP applications require an Enterprise plan with Spectrum as a paid add-on").
- SQL Server: Change Tracking
  https://learn.microsoft.com/sql/relational-databases/track-changes/about-change-tracking-sql-server
  (primary key values are what is recorded); CDC
  https://learn.microsoft.com/sql/relational-databases/track-changes/about-change-data-capture-sql-server
  ("Change data capture utilizes the SQL Server Agent").
- Chrome Local Network Access: https://developer.chrome.com/blog/local-network-access (prompt
  launching in Chrome 142, secure contexts only, `targetAddressSpace: "local"`, replaces PNA).
- Let's Encrypt profiles: https://letsencrypt.org/docs/profiles/ (`shortlived` 160 h, classic
  90 days).
