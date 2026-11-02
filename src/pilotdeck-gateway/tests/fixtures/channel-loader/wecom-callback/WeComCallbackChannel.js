// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real wecomCallback adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('wecomCallback')) {
  throw new Error(probe.importFails('wecomCallback'));
}
export class WeComCallbackChannel {
  channelKey = 'wecomCallback';
  constructor(options = {}) {
    if (probe) probe.construct('wecomCallback', options);
    const failure = probe ? probe.constructorFails('wecomCallback') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
