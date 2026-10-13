import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { resolve, extname, sep } from 'node:path';
import { root, requireUI, origin, port } from './env.mjs';
const {WebSocketServer} = requireUI('ws');
export async function startMock() {
  const stats = { connections: [], outbound: [], sent: [], requests: [], streams: [], errors: [] };
  const sockets = new Set();
  const intervals = new Set();
  let projectsRevision = 1;
  const server = http.createServer(async (req,res)=>{
    const url = new URL(req.url, origin);
    const json = (value,status=200)=>{res.writeHead(status,{'Content-Type':'application/json'});res.end(JSON.stringify(value));};
    stats.requests.push({path:url.pathname, method:req.method, authorization:req.headers.authorization || null});
    if(url.pathname==='/api/auth/status') return json({needsSetup:false,authDisabled:false});
    if(url.pathname==='/api/auth/login') {
      let body='';for await(const chunk of req)body+=chunk;
      const credentials=JSON.parse(body);
      return credentials.username==='probe' && credentials.password==='probe-secret' ? json({token:'f1-probe-token',user:{id:1,username:'probe',is_admin:true}}) : json({error:'Invalid credentials'},401);
    }
    if(url.pathname==='/api/auth/user') return json({user:{id:1,username:'probe',is_admin:true}});
    if(url.pathname==='/api/user/onboarding-status') return json({hasCompletedOnboarding:true,configuration:{state:'ready',modelRef:'probe/echo',revision:'1'},gateway:{state:'unmanaged'}});
    if(url.pathname==='/api/projects') return json([{name:'probe',path:'/probe',displayName:`Probe ${projectsRevision}`,sessions:[{id:'s',summary:'F1 session',provider:'pilotdeck'}]}]);
    if(url.pathname==='/api/projects/probe/sessions') return json({sessions:[{id:'s',summary:'F1 session',provider:'pilotdeck'}],total:1,hasMore:false});
    if(url.pathname==='/__probe/projects') { projectsRevision++; broadcast({type:'projects_updated'});return json({projectsRevision}); }
    if(url.pathname==='/__probe/close') { for(const s of sockets) if(s.path === (url.searchParams.get('channel') || '/ws')) s.close(); return json({ok:true}); }
    if(url.pathname==='/__probe/stream') {
      const hz=Number(url.searchParams.get('hz')||20), count=Number(url.searchParams.get('count')||1200);
      if(hz < 20 || hz > 100 || count > 10000) return json({error:'bounded workload'},400);
      let seq=0; const started=performance.now();
      const timer=setInterval(()=>{
        seq++; const frame={type:'stream_delta',seq,text:`${String(seq).padStart(4,'0')} 中文增量 frame\n`};
        const t=performance.now();stats.sent.push({seq,t});broadcast(frame);
        if(seq===count) { clearInterval(timer);intervals.delete(timer);stats.streams.push({hz,count,elapsedMs:performance.now()-started}); }
      },1000/hz);intervals.add(timer);return json({hz,count,started});
    }
    if(url.pathname==='/__probe/stats') return json(stats);
    const prefix = url.pathname.match(/^\/probe\/(leptos|dioxus)(?:\/|$)/);
    let path;
    if(prefix) {
      const directory=resolve(root,`_build/dist-${prefix[1]}`);
      const relative=url.pathname.slice(prefix[0].length);
      path=resolve(directory,relative||'index.html');
      if(!path.startsWith(directory+sep)) return json({error:'path'},400);
    } else if(url.pathname.startsWith('/shared/')) {
      const directory=resolve(root,'ui-rs/shared');path=resolve(directory,url.pathname.slice(8));
      if(!path.startsWith(directory+sep)) return json({error:'path'},400);
    } else if(url.pathname==='/vendor/xterm.js') path=requireUI.resolve('@xterm/xterm');
    else if(url.pathname==='/vendor/xterm.css') path=requireUI.resolve('@xterm/xterm/css/xterm.css');
    else return json({error:'Probe mock, no real product page'},404);
    try {
      const data=await readFile(path);
      const mime={'.wasm':'application/wasm','.js':'text/javascript','.html':'text/html','.css':'text/css'};
      res.writeHead(200,{'Content-Type':mime[extname(path)]||'application/octet-stream'});res.end(data);
    } catch { json({error:'missing asset'},404); }
  });
  const wss=new WebSocketServer({server});
  wss.on('error',error=>stats.errors.push(error.message)); // HTTP listen errors also reject startMock().
  wss.on('connection',(socket,req)=>{
    const url=new URL(req.url,origin);socket.path=url.pathname;sockets.add(socket);
    stats.connections.push({path:url.pathname,token:url.searchParams.get('token'),t:performance.now()});
    socket.on('close',()=>sockets.delete(socket));
    socket.on('message',data=>{
      const msg=JSON.parse(data.toString());stats.outbound.push({...msg,path:socket.path,t:performance.now()});
      if(socket.path==='/shell' && msg.type==='input') socket.send(JSON.stringify({type:'output',data:msg.input}));
    });
  });
  function broadcast(frame) { for(const socket of sockets)if(socket.path==='/ws' && socket.readyState===1)socket.send(JSON.stringify(frame)); }
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(port,'127.0.0.1',resolve);});
  return { stats, close: async()=>{
    for(const timer of intervals)clearInterval(timer);
    for(const socket of sockets)socket.terminate();
    await new Promise(r=>wss.close(r));await new Promise(r=>server.close(r));
  } };
}
if(process.argv[1] === new URL(import.meta.url).pathname) {
  const mock=await startMock();console.log(`F1 mock ${origin}`);
  for(const signal of ['SIGINT','SIGTERM'])process.once(signal,async()=>{await mock.close();process.exit(0);});
}
