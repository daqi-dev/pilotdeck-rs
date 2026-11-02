// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real webhook adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('webhook')) {
  throw new Error(probe.importFails('webhook'));
}
export class WebhookChannel {
  channelKey = 'webhook';
  constructor(options = {}) {
    if (probe) probe.construct('webhook', options);
    const failure = probe ? probe.constructorFails('webhook') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
