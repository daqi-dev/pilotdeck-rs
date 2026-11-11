// Normal U9/U10 consumption reuses the registered official client driver.
// This extension exercises only ordinary portable transcript export/restore.
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const [baseline, , project, , endpoint, token, output] = process.argv.slice(2);
assert(baseline && project && endpoint && token && output);
await import(new URL('../../../../../scripts/gateway-parity/session_history_driver.mts', import.meta.url).href);
const u9u10 = JSON.parse(readFileSync(output + '/result.json', 'utf8'));
assert(u9u10.closed === true);
const { GatewayWsClient } = await import(pathToFileURL(baseline + '/src/gateway/index.js').href);
const client = new GatewayWsClient({ url: endpoint, token, clientName: 'web' });
const sourceKey = 'web:normal-history-portable-source';
const restoredKey = 'web:normal-history-portable-restored';
const archive = {
  schemaVersion: 1, format: 'portable_text_messages', title: 'Normal history restore',
  messages: [{ role: 'user', text: 'NORMAL_HISTORY_RESTORE_USER' },
             { role: 'assistant', text: 'NORMAL_HISTORY_RESTORE_ASSISTANT' }],
};
const result: Record<string, any> = {};
const deadline = setTimeout(() => { client.close(); throw new Error('normal transcript restore deadline'); }, 30000);
try {
  await client.connect();
  result.liveExport = await client.request('export_session_transcript', {
    projectKey: project, sessionKey: 'web:session-domain-u9',
  });
  assert(JSON.stringify(result.liveExport).includes('SESSION_DOMAIN_U9_LIVE'));
  assert(JSON.stringify(result.liveExport).includes('GP_MOCK_OK'));
  result.sourceRestore = await client.request('restore_session_transcript', {
    projectKey: project, sessionKey: sourceKey, archive,
  });
  result.sourceExport = await client.request('export_session_transcript', { projectKey: project, sessionKey: sourceKey });
  assert.deepEqual(result.sourceExport, archive);
  result.restore = await client.request('restore_session_transcript', {
    projectKey: project, sessionKey: restoredKey, archive: result.sourceExport,
  });
  result.restoredExport = await client.request('export_session_transcript', {
    projectKey: project, sessionKey: restoredKey,
  });
  assert.deepEqual(result.restoredExport, archive);
  result.history = await client.request('read_session_messages', {
    projectKey: project, sessionKey: restoredKey, limit: 1000,
  });
  const readable = result.history.messages.filter((row: any) => row.kind === 'text').map(
    (row: any) => ({ role: row.role, text: row.text }));
  assert.deepEqual(readable, archive.messages);
  assert(!result.history.stream?.active);
  result.pass = true;
} catch (error) {
  result.error = String(error);
  throw error;
} finally {
  client.close();
  await new Promise(resolve => setTimeout(resolve, 100));
  clearTimeout(deadline);
  writeFileSync(output + '/restoration.json', JSON.stringify(result, null, 2) + '\n', { flag: 'wx' });
}
