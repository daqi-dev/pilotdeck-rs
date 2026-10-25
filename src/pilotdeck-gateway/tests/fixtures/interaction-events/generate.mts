// Execute the complete, unmodified baseline module; no source extraction.
import { execFileSync } from "node:child_process";
const baseline = process.env.PDRS_BASELINE ?? '/Users/da/ws/pilotdeck-rs/resources/PilotDeck';
const head = execFileSync("git", ["-C", baseline, "rev-parse", "HEAD"], {encoding: "utf8"}).trim();
if (head !== "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85") throw new Error("Unexpected baseline HEAD");
if (execFileSync("git", ["-C", baseline, "status", "--porcelain", "--", "src/agent/protocol/events.ts"], {encoding: "utf8"}).trim()) throw new Error("Modified baseline events module");
const { createAgentEventBuffer } = await import(`${baseline}/src/agent/protocol/events.ts`);
const buffer = createAgentEventBuffer();
const first = {type: 'text_delta', sessionId: 'session', turnId: 'turn', text: '中文'};
const second = {type: '', sessionId: 'session'};
buffer.emitter(first as never);
buffer.emitter(second as never);
console.log(JSON.stringify([buffer.drain(), buffer.drain()]));
