// #979 Canonical SQLite bytes for golden oracles.
//
// Several EdgeClaw fixtures embed raw `node:sqlite` database files base64 in
// `oracle.json`. SQLite stores, at header offset 96, the version number of the
// library that last wrote the file. Homebrew's Node links `node:sqlite` against
// a shared `libsqlite3`, so two Node v22.22.0 hosts can write the same logical
// database with different version fields (3.50.4 on the macmini runner,
// 3.53.2 here). A regeneration guard compares whole files, so that four-byte
// informational field turned every database-bearing oracle into a per-host
// golden instead of a per-content golden.
//
// The fix is deliberately narrow: validate the SQLite header, then rewrite
// exactly the four version bytes to the pinned baseline version. Every other
// byte, including the page-size/journal bytes and the entire page content,
// remains whatever the real TS baseline produced, so a genuine content change
// is still a byte mismatch. This is not a mask on the JSON field: the oracle
// keeps the full database bytes and the Rust comparison stays byte exact.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';

/** Baseline SQLite reported by the pinned Node build and recorded in oracles. */
export const CANONICAL_SQLITE_VERSION = '3.51.2';
const CANONICAL_SQLITE_VERSION_NUMBER = 3051002;

/** Page 1 starts after this fixed database header. */
const SQLITE_HEADER_BYTES = 100;
const SQLITE_VERSION_OFFSET = 96;
const SQLITE_MAGIC = Buffer.from('SQLite format 3\0', 'latin1');

/**
 * SQLite versions whose file layout was verified against the committed
 * corpus (3.50.2 and 3.53.2 compared byte-for-byte after the same SQL replay,
 * differing only in the header version; 3.51.x is between them and is the
 * pinned baseline). A new series fails closed so it cannot silently redefine
 * the golden bytes.
 */
const SUPPORTED_SQLITE = /^3\.(?:50|51|53)\.\d+$/;

export function assertSupportedSqliteVersion(version, label = 'SQLite') {
  assert.match(
    version ?? '',
    SUPPORTED_SQLITE,
    `#979 ${label} ${version} is outside the verified 3.50/3.51/3.53 file-format series`,
  );
  return version;
}

/**
 * Return the four-byte header version after validating that `bytes` is a
 * well-formed SQLite 3 database. This reads the version the writer recorded;
 * the canonicaliser below replaces it.
 */
export function sqliteHeaderVersion(bytes) {
  assert.ok(
    Buffer.isBuffer(bytes),
    '#979 sqlite bytes must be a Buffer',
  );
  assert.ok(
    bytes.length >= SQLITE_HEADER_BYTES,
    `#979 sqlite database is shorter than the ${SQLITE_HEADER_BYTES}-byte header`,
  );
  assert.ok(
    bytes.subarray(0, SQLITE_MAGIC.length).equals(SQLITE_MAGIC),
    '#979 sqlite database header magic is not "SQLite format 3"',
  );
  const write = bytes[18];
  const read = bytes[19];
  assert.ok(
    (write === 1 || write === 2) && (read === 1 || read === 2),
    `#979 sqlite file format read/write version must be 1 or 2, got ${write}/${read}`,
  );
  const pageSize = bytes.readUInt16BE(16);
  assert.ok(
    pageSize === 1 || (pageSize >= 512 && (pageSize & (pageSize - 1)) === 0),
    `#979 sqlite page size is invalid: ${pageSize}`,
  );
  return bytes.readUInt32BE(SQLITE_VERSION_OFFSET);
}

/**
 * Copy `bytes` with only the SQLite writer-version field replaced by the
 * pinned baseline. Any other difference is preserved, so the caller's byte
 * comparison still sees it.
 */
export function canonicalSqliteBytes(bytes) {
  const version = sqliteHeaderVersion(bytes);
  assert.ok(
    version >= 3_000_000 && version < 4_000_000,
    `#979 sqlite writer version is implausible: ${version}`,
  );
  const text = `${Math.floor(version / 1_000_000)}.${Math.floor(version / 1_000) % 1_000}.${version % 1_000}`;
  assertSupportedSqliteVersion(text, 'SQLite writer');
  const canonical = Buffer.from(bytes);
  canonical.writeUInt32BE(CANONICAL_SQLITE_VERSION_NUMBER, SQLITE_VERSION_OFFSET);
  return canonical;
}

/** Read a database file and return the canonical bytes as base64. */
export function canonicalSqliteBase64(path) {
  return canonicalSqliteBytes(readFileSync(path)).toString('base64');
}
