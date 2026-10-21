// Inputs for generate.mts. Every expected value is recorded from the real TS
// baseline; this file only lists what to run.
//
// `$TLS` / `$PLAIN` / `$UPGRADE` are the loopback mock origins (127.0.0.1 passes the
// baseline's two-label hostname check). Replies are consumed in order, one
// per request; a scenario fails if any is left over.

const html = (body) => ({ status: 200, headers: { 'content-type': 'text/html; charset=utf-8' }, body });
const text = (body, type = 'text/plain; charset=utf-8') => ({ status: 200, headers: { 'content-type': type }, body });
const json = (value, status = 200, extra = {}) => ({ status, headers: { 'content-type': 'application/json', ...extra }, body: JSON.stringify(value) });
const redirect = (status, location) => ({ status, headers: location === undefined ? {} : { location }, body: '' });

export const urls = [
  'https://example.com', 'http://example.com/a?b#c', 'https://localhost/x', 'https://127.0.0.1/', 'https://10.0.0.1/',
  'https://[::1]/', 'https://user:pass@example.com/', 'https://user@example.com/', 'ftp://example.com/', 'not a url',
  'https://a./', 'https://.a.b/', 'https://a..b/', `https://example.com/${'x'.repeat(1980)}`, `https://example.com/${'x'.repeat(1981)}`, `https://example.com/${'x'.repeat(1982)}`,
  'https://例子.测试/', 'https://EXAMPLE.com:443/', 'http://example.com:80/', 'javascript:alert(1)', 'https://192.168.1.1:8080/x',
];

export const upgrades = [
  'http://a.com:443/x', 'http://a.com:80/x', 'http://a.com:8080/x?q#f', 'HTTP://A.COM', 'http://[::1]/', 'ftp://a.b/', 'http://a.b.',
  'https://a.b/%7Efoo', 'http://a.b/ sp',
];

export const redirects = [
  ['https://www.a.com/', 'https://a.com/x'], ['https://a.com/', 'https://www.a.com/x'], ['https://a.com:1/', 'https://a.com:2/'],
  ['https://www.www.a.com/', 'https://a.com/'], ['https://a.com/', 'http://a.com/'], ['https://a.com/', 'https://u:p@a.com/'],
  ['https://A.com/', 'https://a.COM/'], ['https://a.com/', 'not a url'], ['https://a.com:443/', 'https://a.com/'],
  ['https://a.com/', 'https://b.com/'],
];

export const preapproved = [
  'https://docs.anthropic.com/x', 'https://sub.docs.anthropic.com/', 'https://en.wikipedia.org/wiki/X', 'https://wikipedia.org/',
  'https://vercel.com/docs/a', 'https://vercel.com/blog', 'https://VERCEL.com/docs', 'https://vercel.com/Docs', 'https://github.com/x',
  'https://gist.github.com/x', 'not a url', 'https://example.com/', 'https://kernel.org.evil.com/', 'https://evilkernel.org/',
];

export const prompts = [
  ['# Doc\n\nbody', 'What is it?', true], ['', '', false], ['x', 'é\ud800', false],
];

export const validations = [
  null, 5, 'x', [], {}, { url: '' }, { url: 5 }, { url: 'https://localhost/' }, { url: 'https://example.com/', mode: 'x' },
  { url: 'https://example.com/', mode: null }, { url: 'https://example.com/' }, { url: 'https://example.com/', prompt: '' },
  { url: 'https://example.com/', prompt: 'q' }, { url: 'https://example.com/', mode: 'raw' }, { url: 'https://example.com/', mode: 'llm', prompt: 5 },
];

// web_fetch end-to-end over the mock. `model` scripts the injected
// secondary model: an array of events, or { throw } after the events.
export const fetches = [
  { name: 'raw-plain-text', input: { url: '$TLS/plain', mode: 'raw' }, replies: [text('hello\nworld\n')] },
  { name: 'raw-json', input: { url: '$TLS/data.json', mode: 'raw' }, replies: [json({ a: 1 })] },
  { name: 'raw-binary', input: { url: '$TLS/img', mode: 'raw' }, replies: [{ status: 200, headers: { 'content-type': 'image/png' }, body: 'PNG...' }] },
  { name: 'raw-octet-xml', input: { url: '$TLS/x', mode: 'raw' }, replies: [{ status: 200, headers: { 'content-type': 'application/octet-stream+xml' }, body: 'x' }] },
  { name: 'raw-no-content-type', input: { url: '$TLS/none', mode: 'raw' }, replies: [{ status: 200, headers: {}, body: 'body' }] },
  { name: 'raw-empty', input: { url: '$TLS/empty', mode: 'raw' }, replies: [text('')] },
  { name: 'raw-utf8-invalid', input: { url: '$TLS/bad', mode: 'raw' }, replies: [{ status: 200, headers: { 'content-type': 'text/plain' }, hex: 'e4b8ad ff 41 e4' }] },
  { name: 'markdown-exactly-100000-units', input: { url: '$TLS/m100k', mode: 'default' }, replies: [text('z'.repeat(100000))] },
  { name: 'raw-truncated', input: { url: '$TLS/big', mode: 'raw' }, hashOnly: true, replies: [text(`${'ab'.repeat(50_001)}😀`)] },
  { name: 'raw-surrogate-cut', input: { url: '$TLS/cut', mode: 'raw' }, hashOnly: true, replies: [text(`${'a'.repeat(99_999)}😀tail`)] },
  { name: 'raw-too-large', input: { url: '$TLS/huge', mode: 'raw' }, replies: [{ status: 200, headers: { 'content-type': 'text/plain' }, repeat: ['x', 10 * 1024 * 1024 + 1] }] },
  { name: 'raw-at-limit', input: { url: '$TLS/limit', mode: 'raw' }, hashOnly: true, replies: [{ status: 200, headers: { 'content-type': 'text/plain' }, repeat: ['y', 10 * 1024 * 1024] }] },
  { name: 'html-turndown-missing', input: { url: '$TLS/page', mode: 'raw' }, replies: [html('<h1>Hi</h1><p>x</p>')], turndownMissing: true },
  { name: 'http-upgraded', input: { url: '$UPGRADE/up', mode: 'raw' }, replies: [text('upgraded')] },
  { name: 'cache-hit', input: { url: '$TLS/cached', mode: 'raw' }, replies: [text('first')], repeatCall: true },
  { name: 'cache-key-is-input', input: { url: '$TLS/cached2?x', mode: 'raw' }, replies: [text('one'), text('two')], secondInput: { url: '$TLS/cached2?x#frag', mode: 'raw' } },
  { name: 'redirect-same-host', input: { url: '$TLS/r1', mode: 'raw' }, replies: [redirect(301, '/r2'), redirect(308, '$TLS/final'), text('landed')] },
  { name: 'redirect-cross-host', input: { url: '$TLS/away', mode: 'raw' }, replies: [redirect(302, 'https://other.example/x')] },
  { name: 'redirect-scheme-change', input: { url: '$TLS/down', mode: 'raw' }, replies: [redirect(307, '$PLAIN/x')] },
  { name: 'redirect-relative-dots', input: { url: '$TLS/a/b/c', mode: 'raw' }, replies: [redirect(301, '../d?q=1'), text('dots')] },
  { name: 'redirect-missing-location', input: { url: '$TLS/nl', mode: 'raw' }, replies: [redirect(302)] },
  { name: 'redirect-empty-location', input: { url: '$TLS/nl0', mode: 'raw' }, replies: [redirect(302, '')] },
  { name: 'redirect-303-not-followed', input: { url: '$TLS/see', mode: 'raw' }, replies: [{ status: 303, headers: { location: '/x', 'content-type': 'text/plain' }, body: 'see other' }] },
  { name: 'redirect-exactly-ten-hops', input: { url: '$TLS/redirect-ten/0', mode: 'raw' }, replies: [...Array.from({ length: 10 }, (_, i) => redirect(302, `/redirect-ten/${i + 1}`)), text('landed after exactly ten redirects')] },
  { name: 'redirect-eleventh-hop-rejected', input: { url: '$TLS/redirect-eleven/0', mode: 'raw' }, replies: Array.from({ length: 11 }, (_, i) => redirect(302, `/redirect-eleven/${i + 1}`)) },
  { name: 'redirect-loop', input: { url: '$TLS/loop', mode: 'raw' }, replies: Array.from({ length: 11 }, () => redirect(302, '/loop')) },
  { name: 'egress-blocked', input: { url: '$TLS/blocked', mode: 'raw' }, replies: [{ status: 403, headers: { 'x-proxy-error': 'blocked-by-allowlist' }, body: 'no' }] },
  { name: 'http-404', input: { url: '$TLS/missing', mode: 'raw' }, replies: [{ status: 404, headers: { 'content-type': 'text/html' }, body: '<html>\n  <body>\tNot   Found\u0000 </body>\n</html>' }] },
  { name: 'http-410-empty', input: { url: '$TLS/gone', mode: 'raw' }, replies: [{ status: 410, headers: {}, body: '' }] },
  { name: 'http-401-binary', input: { url: '$TLS/auth', mode: 'raw' }, replies: [{ status: 401, headers: { 'content-type': 'application/pdf' }, body: '%PDF' }] },
  { name: 'preview-exactly-500-units', input: { url: '$TLS/p500', mode: 'default' }, replies: [text('x'.repeat(500))] },
  { name: 'http-418-long-preview', input: { url: '$TLS/teapot', mode: 'raw' }, replies: [{ status: 418, headers: { 'content-type': 'text/plain' }, body: `${'w '.repeat(260)}😀${'z'.repeat(20)}` }] },
  { name: 'http-custom-reason', input: { url: '$TLS/reason', mode: 'raw' }, replies: [{ status: 400, reason: 'Quite Bad', headers: {}, body: 'b' }] },
  { name: 'http-429-retry-after', input: { url: '$TLS/rate', mode: 'raw' }, replies: [{ status: 429, headers: { 'retry-after': '0' }, body: 'slow' }, { status: 429, headers: { 'retry-after': '0' }, body: 'slow' }, { status: 429, headers: { 'retry-after': '7', 'content-type': 'text/plain' }, body: 'slow down' }] },
  { name: 'http-503-then-ok', input: { url: '$TLS/flaky', mode: 'raw' }, replies: [{ status: 503, headers: { 'retry-after': '0' }, body: '' }, text('recovered')] },
  { name: 'invalid-url', input: { url: 'https://localhost/x', mode: 'raw' }, replies: [] },
  { name: 'llm-no-model', input: { url: '$TLS/doc', prompt: 'Summarize' }, replies: [text('doc body')] },
  { name: 'llm-text', input: { url: '$TLS/doc', prompt: 'Summarize' }, replies: [text('doc body')], modelVia: 'context', model: [{ type: 'text_delta', text: 'Sum' }, { type: 'usage', usage: { inputTokens: 1 } }, { type: 'text_delta', text: 'mary' }] },
  { name: 'llm-preapproved', input: { url: 'https://docs.anthropic.com/x', prompt: 'Q' }, replies: [], cacheSeed: { url: 'https://docs.anthropic.com/x', content: 'seeded docs' }, model: [{ type: 'text_delta', text: 'ok' }] },
  { name: 'llm-blank', input: { url: '$TLS/doc', prompt: 'Q' }, replies: [text('d')], model: [{ type: 'text_delta', text: ' \n\t' }] },
  { name: 'llm-error-event', input: { url: '$TLS/doc', prompt: 'Q' }, replies: [text('d')], model: [{ type: 'error', error: { provider: 'p', protocol: 'openai', code: 'rate_limit', message: 'slow', retryable: true, status: 429, retryAfterMs: 5, userHint: undefined } }] },
  { name: 'llm-throw-error', input: { url: '$TLS/doc', prompt: 'Q' }, replies: [text('d')], model: { events: [], throw: { error: true, message: 'boom', code: 'E1', status: 500, retryable: false } } },
  { name: 'llm-throw-canonical', input: { url: '$TLS/doc', prompt: 'Q' }, replies: [text('d')], model: { events: [], throw: { error: true, message: 'wrapped', error_: { provider: 'p', protocol: 'anthropic', code: 'c', message: 'm', retryable: false } } } },
  { name: 'llm-throw-string', input: { url: '$TLS/doc', prompt: 'Q' }, replies: [text('d')], model: { events: [], throw: { error: false, value: 'plain' } } },
  { name: 'llm-aborted', input: { url: '$TLS/doc', prompt: 'Q' }, replies: [text('d')], model: [{ type: 'text_delta', text: 'a' }], abortDuringModel: true },
];

// web_search over the mock. Every request is recorded (method, path, query,
// headers, body) and compared.
const tavilyOk = json({ answer: 'forty-two', results: [{ title: 'T1', url: 'https://t/1', content: 'c1' }, { url: 'https://t/2' }, { title: 'T3' }] });
const glmOk = json({ search_result: [{ title: 'G', link: 'https://g', content: 'gc', media: 'site', publish_date: '2026' }, { name: 'N', href: 'h', description: 'd' }] });
export const searches = [
  { name: 'glm', options: { provider: 'glm', apiKey: 'k1', endpoint: '$TLS/glm' }, input: { query: '  rust  ' }, replies: [glmOk] },
  { name: 'glm-env-key', options: { provider: 'glm', endpoint: '$TLS/glm' }, env: { ZAI_API_KEY: ' zk ' }, input: { query: 'q' }, replies: [glmOk] },
  { name: 'glm-env-endpoint', options: { apiKey: 'k1' }, env: { GLM_WEB_SEARCH_ENDPOINT: '$TLS/env-glm' }, input: { query: 'q' }, replies: [json({ data: { results: [{ title: 'nested' }] } })] },
  { name: 'glm-proxy-code', options: { provider: 'glm', apiKey: 'k', endpoint: '$TLS/glm' }, input: { query: 'q' }, replies: [json({ code: 1001, msg: 'quota' })] },
  { name: 'glm-error-field', options: { provider: 'glm', apiKey: 'k', endpoint: '$TLS/glm' }, input: { query: 'q' }, replies: [json({ error: 'bad key' })] },
  { name: 'glm-http-500', options: { provider: 'glm', apiKey: 'k', endpoint: '$TLS/glm' }, input: { query: 'q' }, replies: [json({ e: 1 }, 500), json({ e: 1 }, 500), { status: 500, headers: {}, body: 'x'.repeat(600) }] },
  { name: 'glm-bad-json', options: { provider: 'glm', apiKey: 'k', endpoint: '$TLS/glm' }, input: { query: 'q' }, replies: [{ status: 200, headers: {}, body: '{"a":' }] },
  { name: 'glm-null-json', options: { provider: 'glm', apiKey: 'k', endpoint: '$TLS/glm' }, input: { query: 'q' }, replies: [{ status: 200, headers: {}, body: 'null' }] },
  { name: 'glm-empty-query', options: { provider: 'glm', apiKey: 'k', endpoint: '$TLS/glm' }, input: { query: '   ' }, replies: [] },
  { name: 'glm-bad-endpoint', options: { provider: 'glm', apiKey: 'k', endpoint: 'ftp://x/' }, input: { query: 'q' }, replies: [] },
  { name: 'no-api-key', options: { provider: 'tavily' }, env: {}, input: { query: 'q' }, replies: [] },
  { name: 'tavily', options: { provider: 'tavily', apiKey: 'tk', endpoint: '$TLS/tavily', organicLimit: 2 }, input: { query: 'life', gl: 'cn' }, replies: [tavilyOk] },
  { name: 'tavily-no-answer', options: { provider: 'tavily', apiKey: 'tk', endpoint: '$TLS/tavily' }, input: { query: 'q' }, replies: [json({ answer: '', results: 'x' })] },
  { name: 'tavily-http-401', options: { provider: 'tavily', apiKey: 'tk', endpoint: '$TLS/tavily' }, input: { query: 'q' }, replies: [{ status: 401, headers: {}, body: 'unauthorized' }] },
  { name: 'serper', options: { provider: 'serper', apiKey: 'sk', endpoint: '$TLS/serper' }, input: { query: 'q', gl: ' us ' }, replies: [json({ organic: [{ title: 'S', link: 'l', snippet: 's', date: 'd' }] })] },
  { name: 'brave', options: { provider: 'brave', apiKey: 'bk', endpoint: '$TLS/brave?x=1&q=old' }, input: { query: 'a b&c', gl: 'de' }, replies: [json({ web: { results: [{ title: 'B', url: 'u', description: 'desc' }] } })] },
  { name: 'brave-missing-web', options: { provider: 'brave', apiKey: 'bk', endpoint: '$TLS/brave' }, input: { query: 'q' }, replies: [json({})] },
  { name: 'baidu', options: { provider: 'baidu', apiKey: 'bd', endpoint: '$TLS/baidu' }, input: { query: '中文' }, replies: [json({ references: [{ title: 'W', url: 'u', content: 'c', type: 'web' }, { title: 'V', type: 'video' }, { name: 'n', snippet: 's' }] })] },
  { name: 'bocha', options: { provider: 'bocha', apiKey: 'bo', endpoint: '$TLS/bocha', organicLimit: 99 }, input: { query: 'q' }, replies: [json({ code: 200, data: { webPages: { value: [{ name: 'BN', url: 'bu', summary: 'bs', siteName: 'site', datePublished: 'dp' }] } } })] },
  { name: 'exa', options: { provider: 'exa', apiKey: 'ex', endpoint: '$TLS/exa', organicLimit: 0.5 }, input: { query: 'q' }, replies: [json({ results: [{ title: 'E', url: 'eu', highlights: ['h1', 5, 'h2'], publishedDate: 'pd' }, { title: 'E2', highlights: [], text: 'tx' }] })] },
  { name: 'serpapi-yahoo', options: { provider: 'serpapi', apiKey: 'a b/c?', endpoint: '$TLS/serp', searchEngine: 'yahoo' }, input: { query: 'q', gl: 'jp' }, replies: [json({ organic_results: [{ title: 'Y', link: 'yl' }] })] },
  { name: 'serpapi-google-gl', options: { provider: 'serpapi', apiKey: 'sp', endpoint: '$TLS/serp' }, input: { query: 'q', gl: 'jp' }, replies: [json({ search_metadata: { status: 'Success' } })] },
  { name: 'serpapi-error-status', options: { provider: 'serpapi', apiKey: 'sp', endpoint: '$TLS/serp' }, input: { query: 'q' }, replies: [json({ search_metadata: { status: 'Error' } })] },
  { name: 'serpapi-bad-engine', options: { provider: 'serpapi', apiKey: 'sp', endpoint: '$TLS/serp', searchEngine: 'ask' }, input: { query: 'q' }, replies: [] },
  { name: 'serpapi-http-key-redacted', options: { provider: 'serpapi', apiKey: 'sec ret', endpoint: '$TLS/serp' }, input: { query: 'q' }, replies: [{ status: 403, headers: {}, body: 'bad key sec ret / sec%20ret / sec+ret' }] },
  { name: 'additional-code-error', options: { provider: 'exa', apiKey: 'ex', endpoint: '$TLS/exa' }, input: { query: 'q' }, replies: [json({ code: '7', message: '   ', msg: 'quota' })] },
  { name: 'additional-error-object', options: { provider: 'exa', apiKey: 'ex', endpoint: '$TLS/exa' }, input: { query: 'q' }, replies: [json({ error: { reason: 'x' } })] },
  { name: 'additional-missing-list', options: { provider: 'bocha', apiKey: 'bo', endpoint: '$TLS/bocha' }, input: { query: 'q' }, replies: [json({ data: {} })] },
  { name: 'additional-array-json', options: { provider: 'exa', apiKey: 'ex', endpoint: '$TLS/exa' }, input: { query: 'q' }, replies: [json([1])] },
  { name: 'custom-post-bearer', options: { provider: 'custom', apiKey: 'ck', endpoint: '$TLS/custom', customProvider: { name: ' Mine ', queryParam: 'term', resultsPath: 'payload.hits', titleField: 'h.t', urlField: 'u' } }, input: { query: 'q', gl: 'fr' }, replies: [json({ payload: { hits: [{ h: { t: 'deep' }, u: 'du', snippet: 'sn' }] } })] },
  { name: 'custom-get-query-key', options: { provider: 'custom', apiKey: 'ck', endpoint: '$TLS/custom?keep=1', customProvider: { method: 'GET', auth: 'queryApiKey', apiKeyParam: 'token' } }, input: { query: 'q' }, replies: [json({ items: [{ title: 't', url: 'u' }] })] },
  { name: 'custom-get-body-key', options: { provider: 'custom', apiKey: 'ck', endpoint: '$TLS/custom', customProvider: { method: 'GET', auth: 'bodyApiKey' } }, input: { query: 'q' }, replies: [json([{ title: 'arr' }])] },
  { name: 'custom-post-body-key', options: { provider: 'custom', apiKey: 'ck', endpoint: '$TLS/custom', customProvider: { auth: 'bodyApiKey', apiKeyParam: 'k' } }, input: { query: 'q' }, replies: [json({ results: [] })] },
  { name: 'custom-auth-none', options: { provider: 'custom', endpoint: '$TLS/custom', customProvider: { auth: 'none' } }, env: {}, input: { query: 'q' }, replies: [json({ webPages: { data: [{ title: 'wp' }] } })] },
  { name: 'custom-no-endpoint', options: { provider: 'custom', apiKey: 'ck', endpoint: '  ' }, input: { query: 'q' }, replies: [] },
  { name: 'custom-bad-endpoint', options: { provider: 'custom', apiKey: 'ck', endpoint: 'mailto:x' }, input: { query: 'q' }, replies: [] },
  { name: 'custom-code-error', options: { provider: 'custom', apiKey: 'ck', endpoint: '$TLS/custom' }, input: { query: 'q' }, replies: [json({ code: 3 })] },
  { name: 'env-provider-order', options: { endpoint: '$TLS/serper-env' }, env: { SERPER_API_KEY: 's', EXA_API_KEY: 'e' }, input: { query: 'q' }, replies: [json({ organic: [] })] },
  { name: 'connection-refused', options: { provider: 'glm', apiKey: 'k', endpoint: '$CLOSED/glm' }, input: { query: 'q' }, replies: [] },
];

export const availability = [
  { options: {}, env: {} },
  { options: { apiKey: ' k ' }, env: {} },
  { options: { provider: 'custom' }, env: { CUSTOM_WEB_SEARCH_API_KEY: 'c' } },
  { options: { provider: 'custom', endpoint: 'https://x/', customProvider: { auth: 'none' } }, env: {} },
  { options: { provider: 'custom', customProvider: { auth: 'none' } }, env: {} },
  { options: {}, env: { BOCHA_API_KEY: 'b' } },
];

// Pure WebFetchUrlCache operations against a pinned Date.now: TTL boundary,
// LRU order on hits, overwrite, eviction by the 50 MiB budget, oversize skip.
const MiB = 1024 * 1024;
const T0 = 1_767_225_600_000;
const TTL = 15 * 60 * 1000;
export const cacheOps = [
  { op: 'set', url: 'a', content: 'A', bytes: 0, at: T0 },
  { op: 'get', url: 'a', at: T0 + TTL - 1 },
  { op: 'get', url: 'a', at: T0 + TTL },
  { op: 'get', url: 'a', at: T0 + TTL + 1 },
  { op: 'set', url: 'b', content: 'B', bytes: 20 * MiB, at: T0 },
  { op: 'set', url: 'c', content: 'C', bytes: 20 * MiB, at: T0 },
  { op: 'get', url: 'b', at: T0 + 1 },
  { op: 'set', url: 'd', content: 'D', bytes: 20 * MiB, at: T0 + 2 },
  { op: 'get', url: 'c', at: T0 + 3 },
  { op: 'get', url: 'b', at: T0 + 3 },
  { op: 'set', url: 'b', content: 'B2', bytes: 5, at: T0 + 4 },
  { op: 'set', url: 'huge', content: 'H', bytes: 50 * MiB + 1, at: T0 + 5 },
  { op: 'set', url: 'exact', content: 'E', bytes: 50 * MiB, at: T0 + 6 },
  { op: 'get', url: 'd', at: T0 + 7 },
  { op: 'get', url: 'exact', at: T0 + 7 },
  { op: 'clear', at: T0 + 8 },
  { op: 'get', url: 'exact', at: T0 + 8 },
];
