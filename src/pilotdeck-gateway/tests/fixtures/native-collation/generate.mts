// Supplemental runtime oracle: all pairs exercise the CE boundary around
// isolated UTF-16. The product-method originals remain #1193's matrix.
import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#132 Node pin');
assert.equal(process.versions.icu, '78.2', '#132 ICU pin');
assert.ok(process.env.PDRS_BASELINE, '#132 baseline required');
assert.ok(process.argv[2], '#132 fresh output directory required');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/gateway/dialog/modelCatalog.ts', 'src/session/transcript/TranscriptReader.ts',
]);
const keys = ['coté', 'côte', 'ax\u0301', 'a\u0301x', '\ud800\ud801\udc00', '', 'a', 'A', 'ä', 'a\u0308', '中', '𠀀', '😀', '􏿿', '\ufffd', '\ufffe', '\uffff',
  '\ud800', '\ud801', '\udbff', '\udc00', '\udc01', '\udfff', '\ue000', '\ue001', '\ue7ff',
  '\ud800\ud801', '\udc00\ud800', '\ud800\udc00', 'a\ud800b', 'a\ud801b', 'a\ue000b', 'a\ufffdb',
  'ä\ud800a', 'a\ud800b', 'a\ud800\u0301', 'a\u0301\ud800', '\ud800\u0308a', '\ud800ä',
  'c\ud800h', 'ch\ud800', 'd\ud800z', 'dz\ud800', 'e\ud800\u0301', 'é\ud800',
  'a\ud800-2', 'a\ud8002', 'a\ud80010', '2\ud8003', '23\ud800',
  'I\ud800\u0307', 'i\ud800\u0307', 'a\ud800\ufffeé', 'á\ud800\ufffee',
  '\u0e40\ud800\u0e01', '\u0e40\u0e01\ud800', '\u1100\ud800\u1161', '\uac00\ud800',
  '\u0301\ud800\u0327', '\u0327\ud800\u0301', 'a\u0000\ud800', 'a\ud800\u0000'];
const locales = ['en-US', 'zh-CN', 'de-DE', 'sv-SE', 'ja-JP', 'fr-CA', 'fr-Latn-CA', 'fr-CA-u-co-emoji', 'fr-CA-u-co-eor', 'es-u-co-trad', 'lt', 'th', 'en-u-kn-true', 'da'];
const rows = locales.map(locale => ({ locale, signs: keys.map(a => keys.map(b => Math.sign(a.localeCompare(b, locale)))) }));
const output = resolve(process.argv[2]);
mkdirSync(output, { recursive: true });
const oracle = { node: process.version, icu: process.versions.icu, sourceCommit, sourceSha256, keys, rows };
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle) + '\n', { flag: 'wx' });
process.stdout.write(JSON.stringify({ node: process.version, icu: process.versions.icu, locales, keys: keys.length, comparisons: keys.length ** 2 * locales.length }) + '\n');
