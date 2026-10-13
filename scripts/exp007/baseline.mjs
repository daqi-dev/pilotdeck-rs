// Read baseline modules in place; never write into resources/PilotDeck.
import {createRequire} from 'node:module';
import {writeFile,mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
import {baseline,requireUI,root,evidence,origin} from './env.mjs';
const viteRequire=createRequire(requireUI.resolve('vite'));
const esbuild=viteRequire('esbuild');
await mkdir(evidence,{recursive:true});
const source=`
import React,{useEffect,useState} from ${JSON.stringify(requireUI.resolve('react'))};
import {createRoot} from ${JSON.stringify(requireUI.resolve('react-dom/client'))};
import {AuthProvider,useAuth} from ${JSON.stringify(`${baseline}/src/components/auth/context/AuthContext.tsx`)};
import LoginForm from ${JSON.stringify(`${baseline}/src/components/auth/view/LoginForm.tsx`)};
import {WebSocketProvider,useWebSocket} from ${JSON.stringify(`${baseline}/src/contexts/WebSocketContext.tsx`)};
import {api} from ${JSON.stringify(`${baseline}/src/utils/api.js`)};
import {BrowserRouter,useNavigate,Link,NavLink} from ${JSON.stringify(requireUI.resolve('react-router-dom'))};
import ${JSON.stringify(`${baseline}/src/i18n/config.js`)};
function SessionProbe(){
 const {token}=useAuth();const {subscribe,sendMessage,isConnected}=useWebSocket();const [list,setList]=useState('');const [received,setReceived]=useState(0);
 useEffect(()=>{if(!token||!isConnected)return;const refresh=async()=>setList(JSON.stringify(await (await api.projects()).json())+'\\n'+JSON.stringify(await (await api.sessions('probe')).json()));refresh();sendMessage({type:'watch-session',sessionId:'s'});return subscribe(f=>{if(f.type==='projects_updated')refresh();if(f.type==='stream_delta')setReceived(f.seq);});},[token,isConnected,subscribe,sendMessage]);
 return <><pre id="sessions">{list}</pre><output id="received">{received}</output></>;
}
function AuthProbe(){const {token,isLoading}=useAuth();return <><LoginForm/><p id="baseline-status">{isLoading?'loading':token?'logged':'ready'}</p>{token&&<SessionProbe/>}</>;}
function NavProbe(){const navigate=useNavigate();return <><button id="navigate" onClick={()=>navigate('/probe/leptos/')}>navigate</button><Link id="link" to="/probe/leptos/">Link</Link><NavLink id="navlink" to="/probe/leptos/">NavLink</NavLink></>;}
const navigation=new URLSearchParams(location.search).has('nav');
createRoot(document.getElementById('main')).render(navigation?<BrowserRouter><NavProbe/></BrowserRouter>:<AuthProvider><WebSocketProvider><AuthProbe/></WebSocketProvider></AuthProvider>);
`;
const outfile=resolve(root,'_build/baseline-probe.js');
await esbuild.build({stdin:{contents:source,loader:'tsx',resolveDir:root},outfile,bundle:true,format:'iife',platform:'browser',jsx:'automatic',jsxImportSource:'react',nodePaths:[`${baseline}/node_modules`],loader:{'.css':'empty','.svg':'dataurl','.png':'dataurl'},define:{'import.meta.env':JSON.stringify({VITE_IS_PLATFORM:'false',VITE_DISABLE_LOCAL_AUTH:'false',DEV:false}),'process.env.NODE_ENV':'"production"'},logLevel:'warning'});
console.log(outfile);
