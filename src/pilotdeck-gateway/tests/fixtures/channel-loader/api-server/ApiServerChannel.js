// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real apiServer adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('apiServer')) {
  throw new Error(probe.importFails('apiServer'));
}
export class ApiServerChannel {
  channelKey = 'apiServer';
  constructor(options = {}) {
    if (probe) probe.construct('apiServer', options);
    const failure = probe ? probe.constructorFails('apiServer') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
