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
  // Algorithm-only expectations under --stack-size=8192, not default-stack
  // end-to-end success claims. On Node 22.22.0 the recorded streamModel chain
  // first emits a RangeError at depth 1230 cold (1292 after warmup); depth 2000
  // sends no HTTP request on the default stack. See README for exact scope.
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

// JSON.parse is intentional: a literal __proto__ would change the input before
// the real TS cleaner/SDK sees it. const/default payloads are retention controls.
const prototypeSchemas = JSON.parse("[{\"name\":\"property-proto-record\",\"schema\":{\"type\":\"object\",\"properties\":{\"__proto__\":{\"type\":\"string\"},\"safe\":{\"type\":\"integer\"}},\"required\":[\"__proto__\",\"type\",\"constructor\",\"safe\",\"missing\"]}},{\"name\":\"property-proto-null\",\"schema\":{\"properties\":{\"__proto__\":null,\"safe\":{\"type\":\"string\"}},\"required\":[\"__proto__\",\"constructor\",\"safe\"]}},{\"name\":\"property-proto-scalar\",\"schema\":{\"properties\":{\"__proto__\":17,\"safe\":{\"type\":\"string\"}},\"required\":[\"__proto__\",\"constructor\",\"safe\"]}},{\"name\":\"schema-proto-array\",\"schema\":{\"__proto__\":{\"type\":\"array\"},\"description\":\"keep\"}},{\"name\":\"schema-proto-properties\",\"schema\":{\"__proto__\":{\"properties\":{\"safe\":{\"type\":\"string\"}}},\"required\":[\"safe\"]}},{\"name\":\"schema-proto-item-inherited\",\"schema\":{\"__proto__\":{\"items\":{\"type\":\"string\"}},\"type\":\"array\"}},{\"name\":\"proto-is-const-payload\",\"schema\":{\"const\":{\"__proto__\":{\"keep\":1},\"safe\":2}}},{\"name\":\"proto-is-default-meta\",\"schema\":{\"anyOf\":[{\"const\":\"x\"},{\"const\":\"y\"}],\"default\":{\"__proto__\":{\"keep\":1},\"safe\":2}}},{\"name\":\"union-proto-no-enum\",\"schema\":{\"anyOf\":[{\"properties\":{\"__proto__\":{\"type\":\"string\"},\"safe\":{\"type\":\"integer\"}},\"required\":[\"__proto__\",\"safe\"]}]}},{\"name\":\"union-proto-enum\",\"schema\":{\"anyOf\":[{\"properties\":{\"__proto__\":{\"enum\":[\"p\"]},\"enum\":{\"type\":\"string\"},\"type\":{\"const\":\"v\"},\"safe\":{\"type\":\"string\"}}}]}},{\"name\":\"union-proto-inherited-schema\",\"schema\":{\"anyOf\":[{\"properties\":{\"__proto__\":{\"const\":{\"__proto__\":{\"data\":1}}},\"safe\":{\"type\":\"string\"}}},{\"properties\":{\"__proto__\":{\"const\":\"q\"},\"safe\":{\"enum\":[\"a\"]}}}]}},{\"name\":\"inherited-required-default\",\"schema\":{\"properties\":{\"safe\":{\"type\":\"integer\"}},\"required\":[\"constructor\",\"__proto__\",\"toString\",\"safe\",\"missing\"]}},{\"name\":\"ref-spread-resets-proto\",\"schema\":{\"$defs\":{\"x\":{\"__proto__\":{\"description\":\"inherited\",\"type\":\"array\"}}},\"$ref\":\"#/$defs/x\",\"description\":\"outer\"}},{\"name\":\"union-spread-resets-proto\",\"schema\":{\"anyOf\":[{\"__proto__\":{\"description\":\"inherited\",\"type\":\"array\"}}],\"description\":\"outer\"}}]");
const prototypeMerges = JSON.parse("[{\"name\":\"merge-source-proto-record\",\"target\":{},\"source\":{\"__proto__\":{\"ghost\":{\"keep\":1}},\"ghost\":{\"live\":2}}},{\"name\":\"merge-source-proto-scalar\",\"target\":{},\"source\":{\"__proto__\":17,\"safe\":1}},{\"name\":\"merge-target-own-proto\",\"target\":{\"__proto__\":{\"ghost\":{\"keep\":1}},\"safe\":1},\"source\":{\"ghost\":{\"live\":2}}},{\"name\":\"merge-source-nested-no-merge-preserves-own\",\"target\":{},\"source\":{\"nested\":{\"__proto__\":{\"keep\":1},\"safe\":2}}},{\"name\":\"merge-source-nested-merge-drops-own\",\"target\":{\"nested\":{\"old\":1}},\"source\":{\"nested\":{\"__proto__\":{\"keep\":1},\"safe\":2}}},{\"name\":\"merge-target-nested-no-merge-preserves-own\",\"target\":{\"nested\":{\"__proto__\":{\"keep\":1},\"safe\":2}},\"source\":{\"other\":1}},{\"name\":\"merge-target-nested-merge-drops-own\",\"target\":{\"nested\":{\"__proto__\":{\"keep\":1},\"safe\":2}},\"source\":{\"nested\":{\"other\":1}}},{\"name\":\"merge-target-proto-null-source-own\",\"target\":{\"__proto__\":null},\"source\":{\"__proto__\":{\"keep\":1},\"safe\":2}}]");
prototypeSchemas.push(...JSON.parse("[{\"name\":\"repeat-inherited-constructor\",\"schema\":{\"anyOf\":[{\"properties\":{\"constructor\":{\"type\":\"string\"},\"toString\":{\"type\":\"boolean\"}},\"required\":[\"constructor\",\"toString\"]},{\"properties\":{\"constructor\":{\"type\":\"number\"},\"toString\":{\"type\":\"integer\"}},\"required\":[\"constructor\",\"toString\"]}]}},{\"name\":\"repeat-inherited-constructor-enum-control\",\"schema\":{\"anyOf\":[{\"properties\":{\"constructor\":{\"type\":\"string\"}}},{\"properties\":{\"constructor\":{\"const\":\"valid\"}}}]}},{\"name\":\"repeat-inherited-constructor-third\",\"schema\":{\"anyOf\":[{\"properties\":{\"constructor\":{\"type\":\"string\"}}},{\"properties\":{\"constructor\":{\"type\":\"number\"}}},{\"properties\":{\"constructor\":{\"type\":\"integer\"}}}]}}]"));
export function recordPrototypeSchemas(normalize: (schema: any) => any, clean: (schema: any) => any) {
  return prototypeSchemas.map((spec: any) => ({ ...spec, cleaned: clean(spec.schema), normalized: normalize(spec.schema) }));
}
export function recordPrototypeMerges(deepMerge: (target: any, source: any) => any) {
  return prototypeMerges.map((spec: any) => ({ ...spec, expected: deepMerge(spec.target, spec.source) }));
}

// Request + SDK serialization also exercise the same cleaner/merge paths.
regressionCases.push({
  name: 'prototype-schema-and-extra-body',
  request: JSON.parse('{"tools":[{"name":"lookup","inputSchema":{"properties":{"__proto__":{"type":"string"},"safe":{"const":"ok"}},"required":["__proto__","type","safe"]}}],"outputSchema":{"name":"answer","schema":{"properties":{"__proto__":null,"safe":{"const":"ok"}},"required":["__proto__","safe"]}}}'),
  provider: JSON.parse('{"extraBody":{"__proto__":{"ghost":{"keep":1}},"ghost":{"live":2},"nested":{"__proto__":{"keep":1},"safe":2}}}'),
  replies: [{ chunks: [{ text: `data: ${JSON.stringify({ candidates: [{ content: { role: 'model', parts: [{ text: 'ok' }] }, finishReason: 'STOP' }] })}\n\n` }] }],
} as any);
