// Loopback parity diagnostic only: make official ws client masking deterministic.
import {createRequire} from "node:module";
const require=createRequire(process.env.PDRS_BASELINE+"/package.json");
const {WebSocket}=require("ws");
globalThis.WebSocket=class extends WebSocket {
 constructor(url,protocols){super(url,protocols,{generateMask:buffer=>buffer.set([1,2,3,4])});}
};
