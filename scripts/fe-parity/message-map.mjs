import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';

const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const linkPattern = /\[`(ui\/[^`]+):(\d+)`\]\(([^)]+)\)/g;
const escape = value => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

// Check content, not just whether a line number is in range. Branch exceptions must
// carry both an exact code anchor and a separately checked literal reference.
export async function checkMessageMap(markdown, baselineRoot) {
  const sources = new Map(), errors = [];
  let links = 0, rows = 0, branches = 0;
  for (const [index, row] of markdown.split('\n').entries()) {
    if (!row.startsWith('|') || !row.includes('[`ui/')) continue;
    rows++;
    const label = row.split('|')[1].trim().replaceAll('`', '');
    const identifier = label.replace(/^.*?(?:type|kind):\s*/, '').split('（')[0];
    const matches = [...row.matchAll(linkPattern)];
    if (matches.length !== (row.match(/\[`ui\//g) || []).length) errors.push(`map:${index + 1}: malformed local line link`);
    for (const match of matches) {
      links++;
      const [, file, number, url] = match;
      const at = `map:${index + 1} ${identifier} ${file}:${number}`;
      if (file.split('/').includes('..') || url !== `https://github.com/OpenBMB/PilotDeck/blob/${commit}/${file}#L${number}`) {
        errors.push(`${at}: link target does not match pinned local location`); continue;
      }
      if (!sources.has(file)) sources.set(file, (await readFile(resolve(baselineRoot, file), 'utf8')).split('\n'));
      const source = sources.get(file)[Number(number) - 1];
      if (!source?.trim()) { errors.push(`${at}: missing/empty source line`); continue; }
      const tail = row.slice(match.index + match[0].length);
      const exception = tail.match(/^（分支起点，无字面量；代码：`([^`]+)`；字面量：/);
      if (exception) {
        branches++;
        const literalLink = tail.slice(exception[0].length).match(new RegExp(`^${linkPattern.source}`));
        if (!literalLink || !source.includes(exception[1]) || /^\s*(?:\/\/|\/\*|\*)/.test(source)) {
          errors.push(`${at}: invalid branch anchor or missing literal reference`);
        }
        // The literal link is also visited normally below; it cannot itself be an exception.
        if (literalLink && !tail.slice(exception[0].length + literalLink[0].length).startsWith('）')) {
          errors.push(`${at}: literal reference must close the branch annotation`);
        }
      } else if (/^\s*(?:\/\/|\/\*|\*)/.test(source) || !new RegExp(`(['"\x60])${escape(identifier)}\\1`).test(source)) {
        errors.push(`${at}: source line lacks declared literal (branch annotation required): ${source.trim()}`);
      }
    }
  }
  if (rows !== 45) errors.push(`message rows ${rows} != 45 (35 main + 4 historical + 6 other)`);
  return { rows, links, branches, errors };
}
