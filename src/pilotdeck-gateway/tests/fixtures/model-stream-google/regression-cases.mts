// Shared by the guarded loopback generator and the repair-round TS probe.
export const regressionCases = [{
  name: 'citation-metadata',
  replies: [{ chunks: [
    { text: `data: ${JSON.stringify({ candidates: [{
      content: { role: 'model', parts: [{ text: 'quoted' }] },
      citationMetadata: {
        citationSources: [{ startIndex: 0, endIndex: 6, uri: 'https://example.invalid/source', license: 'CC-BY', extra: 'preserved' }],
        citations: ['discard'], extra: 'discard',
      },
    }] })}\n\n` },
    { text: `data: ${JSON.stringify({ candidates: [{
      citationMetadata: { citationSources: null, extra: 'discard' }, finishReason: 'STOP',
    }] })}\n\n` },
  ] }],
}, {
  name: 'tool-name-object',
  request: { messages: [
    { role: 'assistant', content: [{ type: 'tool_call', id: 'opaque', name: { toString: null }, input: {} }] },
    { role: 'user', content: [{ type: 'tool_result', toolCallId: 'opaque', content: [{ type: 'text', text: 'result' }] }] },
  ] },
  replies: [{ chunks: [{ text: `data: ${JSON.stringify({ candidates: [{ content: { role: 'model', parts: [{ text: 'ok' }] }, finishReason: 'STOP' }] })}\n\n` }] }],
}];

// Compact descriptors avoid a giant nested JSON fixture. The expected leaf
// and traversed depth are measured from the real TS cleaner, never Rust.
export function recordDeepSchemas(normalize: (schema: any) => any, clean: (schema: any) => any) {
  // Run the generator with --stack-size=8192 for stable deep expected values.
  return [128, 512, 1200, 2000].map((depth) => {
    let schema: any = { const: 'leaf' };
    for (let i = 0; i < depth; i++) schema = { properties: { a: schema } };
    let cleaned = normalize(schema), traversed = 0;
    while (cleaned?.properties?.a) { cleaned = cleaned.properties.a; traversed++; }
    let union: any = { const: 'leaf' };
    for (let i = 0; i < depth; i++) union = { anyOf: [union] };
    return { depth, traversed, leaf: cleaned, union: clean(union) };
  });
}
