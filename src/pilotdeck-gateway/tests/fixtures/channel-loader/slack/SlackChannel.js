// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real slack adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('slack')) {
  throw new Error(probe.importFails('slack'));
}
export class SlackChannel {
  channelKey = 'slack';
  constructor(options = {}) {
    if (probe) probe.construct('slack', options);
    const failure = probe ? probe.constructorFails('slack') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
