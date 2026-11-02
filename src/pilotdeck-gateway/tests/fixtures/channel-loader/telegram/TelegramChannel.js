// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real telegram adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('telegram')) {
  throw new Error(probe.importFails('telegram'));
}
export class TelegramChannel {
  channelKey = 'telegram';
  constructor(options = {}) {
    if (probe) probe.construct('telegram', options);
    const failure = probe ? probe.constructorFails('telegram') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
