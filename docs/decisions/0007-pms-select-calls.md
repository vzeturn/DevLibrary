# 0007. dl-agent-pms: `select` calls on declared tables

- Status: accepted
- Date: 2026-11-21
- Scope: `crates/dl-agent-pms` (ops manifest, SQL executor)
- Request: RT-0614 (for SMILE-PMS RT-0474, the availability screen)

## Context

The agent runs only what its embedded ops manifest names: until now, calls of allow-listed
vendor procedures with bound parameters. SMILE-PMS's availability screen (RT-0474) uses the
vendor procedure `AvailabilityChartNew`, which fails on site SR with SQL error 8152 (string
or binary data would be truncated) as soon as a room type code is longer than two
characters. The procedure belongs to the vendor; we cannot fix it. The owner chose to let
the agent read the tables directly, under the same rules as procedure calls: only tables
and columns the signed agent binary declares, and values only through bind parameters.

## Decision

1. **The manifest declares tables.** A new optional top-level key `tables` lists
   `{"name": "<table>", "columns": ["<col>", ...]}`. Names are bare identifiers
   (`^[A-Za-z_][A-Za-z0-9_]*$`), unique (case-insensitive), each with at least one unique
   column. Table names matching the agent's table floor `^sys`, `^#` or
   `^INFORMATION_SCHEMA$` (case-insensitive) are refused whatever the manifest says, next to
   the existing procedure floor. The schema is always `dbo`.
2. **A call is a procedure call or a select.** A call has exactly one of `proc` (with
   `params`) or `select`:

   ```json
   {"select": {
     "table": "Room",
     "columns": ["RoomNo", "RoomType", "Status"],
     "where": [{"column": "RoomType", "op": "=",
                "param": {"name": "roomType", "type": "varchar", "arg": "roomType",
                          "size": 10, "nullable": false}}],
     "order_by": ["RoomNo"],
     "max_rows": 2000}}
   ```

   The table must be declared; every column in `columns`, `where` and `order_by` must be a
   declared column of that table (case-insensitive); `columns` is non-empty and has no
   duplicates. `op` is one of `=`, `<>`, `<`, `<=`, `>`, `>=`. A `param` has the shape and
   bind rules of a procedure parameter (bindable type, exactly one of `arg` or `value`),
   `output` is not allowed, and parameter names are unique within the call. `where` entries
   are joined with `AND`; `order_by` is ascending. `max_rows` is an integer from 1 to the
   executor's rows-per-set cap (10 000).
3. **Only read ops may select.** A select call in an op of kind `write` is refused.
4. **Everything is checked at manifest parse**, with the same strict decoding as the rest
   of the manifest (case-insensitive keys, unknown keys refused), so a bad manifest stops
   the agent at boot instead of failing an op later.
5. **The SQL text comes from the manifest only.** After validation the statement is built as
   `SELECT TOP (<max_rows>) [c1], [c2] FROM [dbo].[<table>] WHERE [col] <op> @<param> AND ...
   ORDER BY [o1], [o2]`: bracket-quoted identifiers that already passed the bare-name check,
   `max_rows` as an integer literal from the manifest, and values only as `@name`
   placeholders bound with the procedure-parameter binder and sent through
   `sp_executesql`. No frame arg ever reaches the text.
6. **Execution is a procedure call's.** The same transaction (rolled back at the end of the
   read), lock timeout, deadline, deadlock retries, row and byte caps and result encoding
   apply; a select produces one result set and no `ReturnValue`. An op whose calls are
   selects is listed in `hello.ops` like any op with agent calls. Args and rows are never
   logged.
7. **The allow-list hash is unchanged.** `allowlist_hash` stays the hash SMILE-PMS computes
   over `procs`; the `tables` list is part of the signed manifest the agent carries, as
   `procs` is.

## Consequences

- SMILE-PMS can serve the availability screen from `Room`/room-type tables without the
  vendor procedure (its op `fo.availability.source` is RT-0614 S2). The agent needs a new
  release to carry a manifest that declares tables; until then `tables` is empty.
- A site's SQL login needs `SELECT` on the declared tables, besides `EXECUTE` on the
  procedures. Column-level reads are limited by the manifest, not by SQL grants.
- Joins, expressions, `OR`, `IN`, `LIKE` and aggregates are not expressible. A read that
  needs them stays a vendor procedure, or a later ADR widens this one.
- A table read sees the vendor's schema directly: a vendor rename of a declared column fails
  the op with a SQL error (`SQL_ERROR`, number 207) instead of silently changing meaning.

## Addendum 2026-11-25 (RT-1129): aggregates and `GROUP BY`

The owner chose to widen this ADR for the Dashboard reads (forecast, revenue, segments) so
the agent returns small tallies instead of thousands of rows.

- A select may carry `aggregates`: `{"fn": "sum"|"count", "column": "<declared>", "as": "<alias>"}`.
  `sum` needs a declared column, `count` may omit it (`COUNT(*)`); `as` is a bare identifier,
  unique among the aliases and the plain `columns`. No other function exists.
- `group_by` lists declared columns. In a grouped select (any aggregate or `group_by`) every
  plain `columns` entry and every `order_by` entry must be one of the `group_by` columns;
  `columns` may be empty when aggregates exist (one total row).
- `date_bucket` lists `group_by` columns taken as `CAST([col] AS date)` (day of a datetime),
  also in `ORDER BY`. It is the only expression allowed, and its text is fixed.
- The statement stays manifest-only text: `SELECT TOP (n) <cols>, SUM([c]) AS [a] FROM [dbo].[t]
  WHERE ... GROUP BY ... ORDER BY ...`. Values still travel only as bind parameters.
- Still not expressible: joins, `OR`, `IN`, `LIKE`, `HAVING`, other expressions.
