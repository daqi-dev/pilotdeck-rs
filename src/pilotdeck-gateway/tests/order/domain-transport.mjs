// Test-only private channel framing; never a client or deployed protocol.
import assert from 'node:assert/strict';
export const pack=record=>Buffer.from(JSON.stringify(record)+'\n');
export class CommitStream {
  constructor(writer) {this.writer=writer;this.buffer=Buffer.alloc(0);}
  feed(chunk) {
    this.buffer=Buffer.concat([this.buffer,chunk]);assert.ok(this.buffer.length<=1048576,'channel bound');
    let end;
    while((end=this.buffer.indexOf(10))!==-1) {
      const row=this.buffer.subarray(0,end);this.buffer=this.buffer.subarray(end+1);
      this.writer.accept(JSON.parse(row.toString('utf8')));
    }
  }
  finish(count) {assert.equal(this.buffer.length,0,'truncated commit');return this.writer.finalize(count);}
}
