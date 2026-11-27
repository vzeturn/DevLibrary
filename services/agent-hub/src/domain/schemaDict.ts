import { z } from 'zod';

/** Largest schema snapshot upload the hub accepts; under D1's ~2 MB row limit so an INSERT never fails after the check. */
export const MAX_SCHEMA_BYTES = 1_900_000;

const ident = z.string().min(1).max(256);
const triple = z.tuple([ident, ident, ident]);

const column = z
  .object({
    name: ident,
    sql_type: z.string().max(64),
    max_length: z.number().int(),
    nullable: z.boolean(),
    identity: z.boolean(),
  })
  .strict();

const index = z.object({ name: ident, primary: z.boolean(), unique: z.boolean(), columns: z.array(ident).max(64) }).strict();

const table = z
  .object({
    schema: ident,
    name: ident,
    est_rows: z.number().int().min(0),
    columns: z.array(column).max(2000),
    indexes: z.array(index).max(500),
  })
  .strict();

const database = z
  .object({
    name: ident,
    schema_hash: z.string().regex(/^[0-9a-f]{64}$/),
    tables: z.array(table).max(5000),
    foreign_keys: z.array(z.object({ name: ident, from: triple, to: triple }).strict()).max(20000),
    routines: z
      .array(z.object({ schema: ident, name: ident, kind: z.enum(['V', 'P', 'FN', 'IF', 'TF', 'TR']) }).strict())
      .max(20000),
  })
  .strict();

/**
 * `dl-agent` schema snapshot (`POST /v1/agent/schema-snapshot`). Structure only: every object is
 * strict, so a field that could carry a table row is refused instead of stored.
 */
export const schemaSnapshotBody = z
  .object({
    agent_version: z.string().min(1).max(64),
    server_version: z.string().max(300),
    databases: z.array(database).min(1).max(20),
  })
  .strict();
export type SchemaSnapshotBody = z.infer<typeof schemaSnapshotBody>;
export type SnapshotDatabase = z.infer<typeof database>;

/**
 * One expected object of a PMS/PDAOrder feature, to compare with the stored dictionary.
 * `optional_reason` marks an object some SMILE builds lack by design (PDAOrder catalog
 * `optional_on_site`, ADR 0184): a site without it is healthy, so it is not a gap.
 */
export const expectedItem = z
  .object({
    feature: z.string().min(1).max(200),
    database: ident,
    schema: ident.default('dbo'),
    table: ident.optional(),
    columns: z.array(ident).max(200).default([]),
    routine: ident.optional(),
    optional_reason: z.string().min(1).max(500).optional(),
  })
  .strict()
  .refine((i) => (i.table === undefined) !== (i.routine === undefined), 'give exactly one of table or routine');
export type ExpectedItem = z.infer<typeof expectedItem>;

export const coverageBody = z.object({ expected: z.array(expectedItem).min(1).max(5000), agent_ids: z.array(z.string()).max(50).optional() }).strict();
export type CoverageBody = z.infer<typeof coverageBody>;
