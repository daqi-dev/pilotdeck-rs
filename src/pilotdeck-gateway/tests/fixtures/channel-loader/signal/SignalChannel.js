// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real signal adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('signal')) {
  throw new Error(probe.importFails('signal'));
}
export class SignalChannel {
  channelKey = 'signal';
  constructor(options = {}) {
    if (probe) probe.construct('signal', options);
    const failure = probe ? probe.constructorFails('signal') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
