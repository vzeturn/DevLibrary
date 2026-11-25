# latency-probe

End-to-end round-trip timing of read-only PMS routes on the `azolo-demo-pms` tenant, which is wired to
dl-agent at SR (RT-1161 plan: `docs/plans/2026-11-25-agent-latency-budget.md`, sections 4, 6, 7).

```sh
# env: AZOLO_DEMO_VIRTUAL_CLERK_LOGIN, _PASSWORD, _TOTP_SECRET, AZOLO_DEMO_POS_CASHIER_PIN
# (DevLibrary cloud kit; on the iMac: set -a; source ~/.config/rt-kits/smile-pms.env; set +a)
node scripts/latency-probe/probe.mjs --samples 200 --gap 150 > run.json
node scripts/latency-probe/probe.mjs --samples 40 --gap 5200 --only fo.inhouse.list   # past the 5 s API cache
node --test scripts/latency-probe/lib.test.mjs
```

- GET routes only. `pos.check.pay` is a write on a live hotel with no dry-run: never add it.
- Bodies hold real guest data: the probe keeps status, size and timing only. Do not print bodies.
- Stops a route after five errors in a row or one sample over 5 s. Be gentle at peak hours.
- `--gap` above 3 s keeps the connection warm with untimed `/health` calls (undici drops idle
  keep-alive after 4 s, which would add a TLS handshake to every sample).
- Numbers are end to end from the caller; subtract the `health` route to get the SaaS -> SR part.
