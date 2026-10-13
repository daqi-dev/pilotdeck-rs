// Only third-party DOM ownership lives here. App state and protocols live in Rust.
export class TerminalIsland {
  constructor() {
    this.root = document.createElement('div');
    this.root.dataset.island = 'xterm';
    this.terminal = new globalThis.Terminal({ cols: 80, rows: 10, allowProposedApi: true });
    this.opened = false;
    this.focused = false;
    this.inputs = 0;
    this.parking = document.createDocumentFragment();
    this.terminal.onData(data => {
      this.inputs++;
      if (this.socket?.readyState === 1) this.socket.send(JSON.stringify({type: 'input', input: data}));
      else this.terminal.write(data); // Local echo is the S2 alternative to a PTY.
    });
  }
  mount(id) {
    const host = document.getElementById(id);
    if (!host) throw new Error(`Missing island host ${id}`);
    host.appendChild(this.root);
    if (!this.opened) { this.terminal.open(this.root); this.opened = true; }
    if (this.focused) this.terminal.focus();
  }
  detach() {
    if (this.root.isConnected && this.root.contains(document.activeElement)) this.focused = true;
    this.parking.appendChild(this.root);
  }
  focus() { this.focused = true; this.terminal.focus(); }
  write(data) { this.terminal.write(data); }
  connect(url) {
    if (this.socket) this.socket.close();
    const socket = new WebSocket(url);
    this.socket = socket;
    socket.onopen = () => socket.send(JSON.stringify({type:'init',projectPath:'/probe',provider:'plain-shell',isPlainShell:true,hasSession:false,cols:80,rows:10}));
    socket.onmessage = e => { const frame=JSON.parse(e.data); if(frame.type==='output') this.terminal.write(frame.data); };
    socket.onclose = () => { this.terminal.clear(); }; // Baseline: no automatic /shell reconnect.
  }
  snapshot() {
    const buffer = this.terminal.buffer.active;
    const lines = [];
    for (let i = 0; i < buffer.length; i++) lines.push(buffer.getLine(i)?.translateToString(true));
    return JSON.stringify({ lines, inputs: this.inputs, focused: this.root.contains(document.activeElement), connected: this.root.isConnected });
  }
}
