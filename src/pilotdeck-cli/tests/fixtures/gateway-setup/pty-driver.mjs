// Direct fixed-baseline setup function; empty credentials must never use HTTP.
globalThis.fetch = async () => { throw new Error('PTY fixture forbids network requests'); };
const { runGatewaySetup } = await import(process.argv[2]);
runGatewaySetup(['feishu']).catch(error => {
  console.error(error.message);
  process.exitCode = 1;
});
