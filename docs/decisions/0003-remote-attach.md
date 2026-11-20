# 0003. Remote attach: products are switched on from the hub

- Status: accepted
- Date: 2026-11-20
- Request: RT-0261 (reopened by the owner: "install the agent once, then each product picks the
  agent on the hub; nobody goes to the customer's server again")

## Context

ADR 0002 lets one agent serve many products, but a second product still needs someone on the
machine: the product's install line (or `dl-agent attach --code`) and then the module's own enrol
tool (`einvoice-enroll`, `pms-enroll`, `pda-enroll`, `arrival-enroll`, ...), which exchanges the
product cloud's one-time code for the module's credential. The demo bench (`install.ps1 -Demo`)
automated this on the machine but still needed a session there; the owner found it too complex.
The agent already polls the hub for signed commands (PROTOCOL §4) and the demo bench table
(`domain/demo.ts`) already says, per module, which tool, variables and flags enrol it.

## Decision

- `POST /admin/agents/:id/attach {module, secret, options, external_id?}` enables
  `modules.<module>` in the agent's config, records the product link, and for a module with an
  enrol tool queues one `attach` command `{module, tool, argv}`. The demo bench table is the
  single source of the allowed tool, variables and flags.
- The product's one-time code travels in `commands.secret` (migration 0010), apart from `args`:
  no admin view or audit row shows it, it is added to the command as `args.env` when the agent
  takes it and cleared from the hub in the same batch, or at expiry.
- The agent's `sys` module runs the tool as a child process of the service with the variables
  set, no input and a 10 minute limit, and answers with the exit status and the masked output.
  It accepts only tools the binary registers and `DL_AGENT_*` variables, and never lets the hub
  change `--data`.
- A successful `attach` makes the hub queue `restart_module` for the module.

## Consequences

- One install per customer; every further product is a hub call. The tools are the same code
  the install line runs, so a remote attach behaves like a local one.
- The hub holds a product's one-time code for at most the command's life (default 1 hour, at most
  1 day) and only until the agent's next heartbeat when it is online. The codes are short-lived
  and single-use at the product cloud.
- A tool that needs an answer at the console (no saved SQL login, for example) fails remotely
  with its own message; the operator supplies the missing value as a variable or option.
- Agents before 1.3.17 do not know `attach` and answer `unknown command`; `update_now` first.

## Later work

- A product key may attach only its own module (today: admin token only).
- A console page to pick an agent and a product instead of the API call.
- Detach: disable a module and drop its link.
