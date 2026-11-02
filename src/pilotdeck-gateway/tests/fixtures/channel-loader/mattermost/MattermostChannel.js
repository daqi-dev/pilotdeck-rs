// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real mattermost adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('mattermost')) {
  throw new Error(probe.importFails('mattermost'));
}
export class MattermostChannel {
  channelKey = 'mattermost';
  constructor(options = {}) {
    if (probe) probe.construct('mattermost', options);
    const failure = probe ? probe.constructorFails('mattermost') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
