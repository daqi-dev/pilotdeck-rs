// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real whatsapp adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('whatsapp')) {
  throw new Error(probe.importFails('whatsapp'));
}
export class WhatsAppChannel {
  channelKey = 'whatsapp';
  constructor(options = {}) {
    if (probe) probe.construct('whatsapp', options);
    const failure = probe ? probe.constructorFails('whatsapp') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
