import { test } from 'node:test';
import assert from 'node:assert/strict';
import { totp, percentile, summarize } from './lib.mjs';

test('totp matches the RFC 6238 SHA-1 vector', () => {
  // seed "12345678901234567890" in base32, T = 59 s -> 94287082 (last 6 digits 287082)
  assert.equal(totp('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 59_000), '287082');
});

test('percentile uses nearest rank', () => {
  const s = Array.from({ length: 100 }, (_, i) => i + 1);
  assert.equal(percentile(s, 50), 50);
  assert.equal(percentile(s, 95), 95);
  assert.equal(percentile(s, 99), 99);
  assert.equal(percentile([], 50), null);
});

test('summarize rounds to 0.1 ms', () => {
  assert.deepEqual(summarize([1.04, 2.06]), { n: 2, p50: 1, p95: 2.1, p99: 2.1 });
});
