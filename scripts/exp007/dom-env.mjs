// Minimal DOM fallback only: no geometry, paint, native OS IME or native scroll proof.
import { readFile } from 'node:fs/promises';
import { requireUI, origin } from './env.mjs';
const {JSDOM, VirtualConsole}=requireUI('jsdom');
const NativeRequest=globalThis.Request;
const NativeEvent=globalThis.Event;
const nativeFetch=globalThis.fetch;
export async function createDOM(path='/probe/leptos/') {
  const errors=[];
  const console=new VirtualConsole();console.on('jsdomError',e=>errors.push(e.message));
  console.on('error',(...args)=>errors.push(args.map(String).join(' ')));
  const dom=new JSDOM('<!doctype html><html><body><div id="main"></div></body></html>',{url:origin+path,pretendToBeVisual:true,runScripts:'outside-only',virtualConsole:console});
  const w=dom.window;
  class BrowserRequest extends NativeRequest { constructor(input,init) { super(typeof input==='string'?new URL(input,w.location.href):input,init); } }
  w.fetch=(input,init)=>nativeFetch(typeof input==='string'?new URL(input,w.location.href):input,init);
  for(const name of ['Request','Response','Headers','AbortController','AbortSignal','TextEncoder','TextDecoder']) w[name]= name==='Request'?BrowserRequest:globalThis[name];
  // jsdom implements WebSocket; therefore the wasm path uses actual local network WS.
  w.ResizeObserver=class { observe() {} unobserve(){} disconnect(){} };
  w.matchMedia=()=>({matches:false,addListener(){},removeListener(){},addEventListener(){},removeEventListener(){}});
  w.HTMLCanvasElement.prototype.getContext=function(){return { canvas:this,measureText:text=>({width:String(text).length*8}),fillRect(){},clearRect(){},fillText(){},getImageData:()=>({data:new Uint8ClampedArray(4)}),createLinearGradient:()=>({addColorStop(){}}),setTransform(){},save(){},restore(){},beginPath(){},rect(){},clip(){},drawImage(){} };};
  const globals=['window','document','navigator','location','localStorage','sessionStorage','Window','Document','Node','Element','HTMLElement','HTMLDivElement','HTMLInputElement','HTMLTextAreaElement','HTMLCanvasElement','KeyboardEvent','CompositionEvent','InputEvent','MouseEvent','MutationObserver','WebSocket','Request','Response','Headers','getComputedStyle','requestAnimationFrame','cancelAnimationFrame','ResizeObserver'];
  for(const name of globals) Object.defineProperty(globalThis,name,{configurable:true,writable:true,value:typeof w[name]==='function' && ['getComputedStyle','requestAnimationFrame','cancelAnimationFrame'].includes(name)?w[name].bind(w):w[name]});
  // Preserve Node/undici's Event brand, while wasm web-sys instanceof Event must
  // also accept jsdom DOM events. Without this, Dioxus discards generic input events.
  globalThis.Event=new Proxy(NativeEvent,{get:(target,key)=>key===Symbol.hasInstance?(value)=>value instanceof NativeEvent || value instanceof w.Event:Reflect.get(target,key)});
  globalThis.self=w;
  w.eval(await readFile(requireUI.resolve('@xterm/xterm'),'utf8'));
  globalThis.Terminal=w.Terminal;
  return {dom,w,errors};
}
export const pause=ms=>new Promise(r=>setTimeout(r,ms));
export async function waitFor(check,timeout=5000) {
  const end=performance.now()+timeout;
  while(performance.now()<end){const value=check();if(value)return value;await pause(10);}
  throw new Error(`DOM condition timeout after ${timeout}ms`);
}
