#!/usr/bin/env node
// Records host JSON.parse SyntaxError messages for json_syntax.rs differential tests.
// Usage: node generate.mjs  (writes cases.json beside this file; Node 22 required)
import { writeFileSync } from "node:fs";

const major = Number(process.versions.node.split(".")[0]);
if (major !== 22) {
  console.error(`Node 22 required (V8 12.x messages); got ${process.version}`);
  process.exit(2);
}

const inputs = [];
const add = (...values) => inputs.push(...values);

// Minus sign and number edges.
add("-", "-x", "--", "-.", "-0", "-01", "01", "1.", "1.e3", ".5", "1e", "1e+", "1E-x", "-Infinity", "+1", "0x10");
// Empty, whitespace, specials and literal mismatch positions.
add("", " ", "\t\n", "undefined", "[object Object]", "Infinity", "NaN", "nan", "True");
add("t", "tx", "tru", "trux", "nul", "nulL", "fals", "falsex", "nullnull", "[tru]", "[nul", "{\"a\":tr}");
// Structural errors, each message kind at least once.
add("{", "[", "{}x", "{} x", "[]]", "{,}", "{\"a\":1,}", "[1,]", "{\"a\"}", "{\"a\" \"b\"}", "{'a':1}",
  "{a:1}", "[1;2]", "[1 2]", "{\"a\":1 \"b\":2}", "{\"a\":1,\"b\"}", "[\"a\" 1]", "{\"a\":{\"b\":[1,{]}}");
// Strings: unterminated, escapes, control characters, \u forms.
add("\"abc", "\"a\\", "\"\\x\"", "\"\\u12\"", "\"\\u12G4\"", "\"\\u\"", "\"\t\"", "\"\u0000\"", "\"\\uD800\"", "[\"\\uDC00\" 1]");
// Non-JSON whitespace before a value, and BOM.
add("\u00a0{}", "\ufeff{}", "\u2028[]", "\u000b1");
// Line/column after CR, LF, CRLF, and U+2028/U+2029 (inside strings) before the error.
add("[\r\n1 2]", "[\r1\n 2]", "[\n\r1 2]", "[\"\u2028\", 1 2]", "[\"a\u2029b\"\n, 1 2]", "{\"a\":1}\n x", "\r\n\r\n{");
// Long numbers and trailing garbage.
add("1".repeat(30) + "x", "[" + "1,".repeat(15) + "]");

// Context window: unexpected token at pos in a source of length len, for both
// sides of the 21-unit threshold and of the pos < 10 / pos < len - 10 splits.
const tail = "abcdefghijklmnopqrstuvwxyz0123456789";
for (const len of [19, 20, 21, 22, 23, 30]) {
  const positions = new Set([0, 1, 8, 9, 10, 11, len - 11, len - 10, len - 9, len - 1]);
  for (const pos of [...positions].filter((p) => p >= 0 && p < len).sort((a, b) => a - b)) {
    add("[".repeat(pos) + "!" + tail.slice(0, len - pos - 1));
  }
}
// Astral characters: the token itself, and window edges splitting surrogate pairs.
add("[\"😀\", 😀]", "😀");
for (let shift = 0; shift < 4; shift += 1) {
  add("[\"" + "a".repeat(shift) + "😀".repeat(6) + "\",!" + "😀".repeat(6));
  add("😀".repeat(shift) + "!" + "😀".repeat(8));
}
// Valid JSON must stay valid.
add("{}", "[]", "0", "-0.5e+3", "\"\"", "\"\\u00e9\\uD83D\\uDE00\"", " {\"k\": [true, false, null]} ", "\"\u2028\"");

const seen = new Set();
const cases = [];
for (const input of inputs) {
  if (seen.has(input)) continue;
  seen.add(input);
  if (!input.isWellFormed()) throw new Error(`argv cannot carry lone surrogates: ${JSON.stringify(input)}`);
  let message = null;
  try {
    JSON.parse(input);
  } catch (error) {
    if (!(error instanceof SyntaxError)) throw error;
    message = error.message;
  }
  // console.error writes UTF-8: a split surrogate in the message becomes U+FFFD.
  cases.push({ input, message: message === null ? null : message.toWellFormed(),
    loneSurrogateInMessage: message !== null && !message.isWellFormed() });
}

const out = new URL("./cases.json", import.meta.url);
writeFileSync(out, JSON.stringify({ node: process.version, v8: process.versions.v8, cases }, null, 1) + "\n");
console.log(`${cases.length} cases from Node ${process.version} (V8 ${process.versions.v8}) -> ${out.pathname}`);
