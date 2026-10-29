// #979 Unit guard for the SQLite byte canonicaliser.
//
// Positive: two databases that differ only in the writer-version field become
// byte-identical. Negative: a single page byte outside that field survives
// canonicalisation, and malformed headers are rejected instead of masked.
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  CANONICAL_SQLITE_VERSION,
  assertSupportedSqliteVersion,
  canonicalSqliteBase64,
  canonicalSqliteBytes,
  sqliteHeaderVersion,
} from './sqlite_bytes.mjs';
import {mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';

function database(version = 3053002, pageByte = 0x11) {
  const bytes = Buffer.alloc(4096);
  Buffer.from('SQLite format 3\0', 'latin1').copy(bytes, 0);
  bytes.writeUInt16BE(4096, 16);
  bytes[18] = 2;
  bytes[19] = 2;
  bytes.writeUInt32BE(version, 96);
  // Page 1 begins at offset 100; this byte is real database content.
  bytes[100] = pageByte;
  bytes[500] = 0xabc & 0xff;
  return bytes;
}

test('#979 canonicalises only the SQLite writer version', () => {
  const older = database(3050004, 0x11);
  const newer = database(3053002, 0x11);
  assert.notDeepEqual(older, newer, '#979 the fixture must start with a real version difference');
  assert.equal(sqliteHeaderVersion(older), 3050004);
  assert.equal(sqliteHeaderVersion(newer), 3053002);
  assert.deepEqual(
    canonicalSqliteBytes(older),
    canonicalSqliteBytes(newer),
    '#979 writer-version differences must not reach the oracle',
  );
  // The canonical field is pinned, not merely made equal to one of the inputs.
  assert.equal(sqliteHeaderVersion(canonicalSqliteBytes(older)), 3051002);
  assert.equal(CANONICAL_SQLITE_VERSION, '3.51.2');
});

test('#979 a page-byte difference outside the header survives', () => {
  const left = canonicalSqliteBytes(database(3050004, 0x11));
  const right = canonicalSqliteBytes(database(3053002, 0x12));
  assert.notDeepEqual(left, right, '#979 content differences must stay observable');
  assert.equal(left[100], 0x11);
  assert.equal(right[100], 0x12);
});

test('#979 rejects malformed database headers instead of masking them', () => {
  const short = Buffer.alloc(99);
  assert.throws(() => canonicalSqliteBytes(short), /#979 sqlite database is shorter/);
  const magic = database();
  magic[0] = 0;
  assert.throws(() => canonicalSqliteBytes(magic), /#979 sqlite database header magic/);
  const format = database();
  format[18] = 3;
  assert.throws(() => canonicalSqliteBytes(format), /#979 sqlite file format read\/write version/);
  const page = database();
  page.writeUInt16BE(1000, 16);
  assert.throws(() => canonicalSqliteBytes(page), /#979 sqlite page size is invalid/);
  const version = database(2_999_999);
  assert.throws(() => canonicalSqliteBytes(version), /#979 sqlite writer version is implausible/);
  const series = database(3054000);
  assert.throws(
    () => canonicalSqliteBytes(series),
    /#979 SQLite writer 3\.54\.0 is outside the verified 3\.50\/3\.51\/3\.53 file-format series/,
  );
});

test('#979 canonicalSqliteBase64 keeps the file content and replaces the version', () => {
  const root = mkdtempSync(join(tmpdir(), 'pdrs-979-sqlite-bytes-'));
  try {
    const path = join(root, 'control.sqlite');
    writeFileSync(path, database(3050004, 0x11));
    const text = canonicalSqliteBase64(path);
    const bytes = Buffer.from(text, 'base64');
    assert.equal(sqliteHeaderVersion(bytes), 3051002);
    assert.equal(bytes[100], 0x11);
  } finally {
    rmSync(root, {recursive: true, force: true});
  }
});

test('#979 accepts the verified series and rejects an unverified one', () => {
  for (const version of ['3.50.4', '3.51.2', '3.53.2']) {
    assert.equal(assertSupportedSqliteVersion(version), version);
  }
  assert.throws(
    () => assertSupportedSqliteVersion('3.54.0'),
    /#979 SQLite 3\.54\.0 is outside the verified 3\.50\/3\.51\/3\.53 file-format series/,
  );
});
