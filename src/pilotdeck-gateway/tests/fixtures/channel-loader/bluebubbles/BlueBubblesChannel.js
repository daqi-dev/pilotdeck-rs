// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real bluebubbles adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('bluebubbles')) {
  throw new Error(probe.importFails('bluebubbles'));
}
export class BlueBubblesChannel {
  channelKey = 'bluebubbles';
  constructor(options = {}) {
    if (probe) probe.construct('bluebubbles', options);
    const failure = probe ? probe.constructorFails('bluebubbles') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
