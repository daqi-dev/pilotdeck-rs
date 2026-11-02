// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real dingtalk adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('dingtalk')) {
  throw new Error(probe.importFails('dingtalk'));
}
export class DingTalkChannel {
  channelKey = 'dingtalk';
  constructor(options = {}) {
    if (probe) probe.construct('dingtalk', options);
    const failure = probe ? probe.constructorFails('dingtalk') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
