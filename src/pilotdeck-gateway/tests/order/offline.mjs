// Fail closed on network attempts; no provider or socket listener is allowed.
import net from 'node:net';
import tls from 'node:tls';
import dgram from 'node:dgram';
import dns from 'node:dns';
import {syncBuiltinESMExports} from 'node:module';
const deny=()=>{throw Error('OWNED_PROBE_EXTERNAL_ACCESS_BLOCKED');};
net.Socket.prototype.connect=deny;net.Server.prototype.listen=deny;
tls.connect=deny;dgram.createSocket=deny;
// Query functions exist on both module surfaces and Resolver prototypes.
// Patch inherited query owners too, including already-created Resolver objects.
const isDnsQuery=name=>name==='lookup'||name==='lookupService'||name==='reverse'||name.startsWith('resolve');
const blockDnsQueries=target=>{
  for(const name of Object.getOwnPropertyNames(target)){
    if(isDnsQuery(name)&&typeof target[name]==='function')target[name]=deny;
  }
};
blockDnsQueries(dns);blockDnsQueries(dns.promises);
for(const Resolver of [dns.Resolver,dns.promises.Resolver]){
  for(let prototype=Resolver.prototype;prototype&&prototype!==Object.prototype;prototype=Object.getPrototypeOf(prototype)){
    blockDnsQueries(prototype);
  }
}
globalThis.fetch=deny;
syncBuiltinESMExports();
