// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real email adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('email')) {
  throw new Error(probe.importFails('email'));
}
export class EmailChannel {
  channelKey = 'email';
  constructor(options = {}) {
    if (probe) probe.construct('email', options);
    const failure = probe ? probe.constructorFails('email') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
