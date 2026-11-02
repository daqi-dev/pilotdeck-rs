// Mock channel module for the channel-loader fixture. The real baseline
// loadEnabledChannels.ts dynamically imports exactly this path; this class
// only records its construction options into the generator's probe. It is
// not a real discord adapter: no network, no account, no external service.
const probe = globalThis.__pdrsChannelLoaderProbe;
if (probe && probe.importFails('discord')) {
  throw new Error(probe.importFails('discord'));
}
export class DiscordChannel {
  channelKey = 'discord';
  constructor(options = {}) {
    if (probe) probe.construct('discord', options);
    const failure = probe ? probe.constructorFails('discord') : undefined;
    if (failure !== undefined && failure !== null) throw failure;
  }
}
