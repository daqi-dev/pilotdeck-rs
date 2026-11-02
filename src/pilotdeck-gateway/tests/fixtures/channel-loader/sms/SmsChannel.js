// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real sms adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('sms')) {
  throw new Error(probe.importFails('sms'));
}
export class SmsChannel {
  channelKey = 'sms';
  constructor(options = {}) {
    if (probe) probe.construct('sms', options);
    const failure = probe ? probe.constructorFails('sms') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
