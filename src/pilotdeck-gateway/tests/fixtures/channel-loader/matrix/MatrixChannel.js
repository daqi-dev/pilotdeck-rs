// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real matrix adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('matrix')) {
  throw new Error(probe.importFails('matrix'));
}
export class MatrixChannel {
  channelKey = 'matrix';
  constructor(options = {}) {
    if (probe) probe.construct('matrix', options);
    const failure = probe ? probe.constructorFails('matrix') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
