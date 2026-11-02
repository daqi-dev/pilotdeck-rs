// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real homeassistant adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('homeassistant')) {
  throw new Error(probe.importFails('homeassistant'));
}
export class HomeAssistantChannel {
  channelKey = 'homeassistant';
  constructor(options = {}) {
    if (probe) probe.construct('homeassistant', options);
    const failure = probe ? probe.constructorFails('homeassistant') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
