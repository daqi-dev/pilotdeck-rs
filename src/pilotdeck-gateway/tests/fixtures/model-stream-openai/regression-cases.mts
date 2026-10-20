const schema = {
  type: 'object',
  properties: { 'bad-name': { type: 'string' }, good: { const: 'x' }, list: { type: 'array', minItems: 1 } },
  required: ['bad-name', 'good', 'missing'], additionalProperties: false, $schema: 'x',
};
const sse = (value: unknown) => ({ text: `data: ${JSON.stringify(value)}\n\n` });
export const regressionCases = [{
  name: 'google-compatible-schema',
  provider: { id: 'google' },
  request: {
    tools: [{ name: 'lookup', description: 'Google schema', inputSchema: schema }],
    outputSchema: { name: 'Answer', schema },
  },
  replies: [{ chunks: [
    sse({ id: 'google-openai', choices: [{ index: 0, delta: { role: 'assistant', content: 'ok' }, finish_reason: null }] }),
    sse({ id: 'google-openai', choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] }),
    { text: 'data: [DONE]\n\n' },
  ] }],
}];
