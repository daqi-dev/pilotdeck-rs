import { writeFileSync, realpathSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join, resolve } from 'node:path';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { pathToFileURL } from 'node:url';
const base = resolve(process.env.PDRS_BASELINE!);
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which',['git'],{encoding:'utf8'}).trim());
const ref = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(base, ref, git);
const sourceSha256 = sourceClosureSha256(base,ref,git,['src/tool/builtin/webFetch.ts','src/tool/builtin/webSearch.ts']);
const {getURLMarkdownContent,__setWebFetchHookForTesting}=await import(pathToFileURL(base+'/src/tool/builtin/web/urlFetcher.ts').href);
const {WebFetchUrlCache,clearWebFetchCache}=await import(pathToFileURL(base+'/src/tool/builtin/web/urlContentCache.ts').href);
const {createWebFetchTool}=await import(pathToFileURL(base+'/src/tool/builtin/webFetch.ts').href);
const {normalizeToolError}=await import(pathToFileURL(base+'/src/tool/protocol/errors.ts').href);
let html = '';
const hook = async () => ({
  status: 200, statusText: 'OK', headers: { 'content-type': 'text/html' },
  arrayBuffer: async () => Buffer.from(html),
});
__setWebFetchHookForTesting(hook);
const run = async () => {
  clearWebFetchCache();
  return await createWebFetchTool({}).execute(
    { url: 'https://example.com/page', mode: 'raw' },
    { cwd: '/w', sessionId: 's', env: {}, abortSignal: new AbortController().signal },
  );
};
// Depth counts HTML/BODY/wrapper and parser-created TBODY (four per table).
const cases = [];
for(const tag of ['ul','ol','div','blockquote','span','table','pre']){
 const [open,close,width]=tag==='ul'||tag==='ol'?[`<${tag}><li>`,`</li></${tag}>`,2]:tag==='table'?['<table><tr><td>','</td></tr></table>',4]:[`<${tag}>`,`</${tag}>`,1];
 for(const depth of [255,256,257]){
  const n=Math.floor((depth-3)/width),pad=depth-3-n*width;
  html=open.repeat(n)+'<span>'.repeat(pad)+'x'+'</span>'.repeat(pad)+close.repeat(n);
  const value=await getURLMarkdownContent('https://example.com/page',new AbortController().signal,{fetchHook:hook,cache:new WebFetchUrlCache()});
  cases.push({name:tag+'-'+depth,depth,html,markdown:value.content});
 }
}
// The review supplied coverage descriptions, not its original 40 inputs.
// These are new real-TS cases covering that same described surface.
const shapes = [
'<ul><li>a<ul><li>b</li><li>c</li></ul></li><li>d</li></ul>',
'<ol start="-2"><li>a<pre><code>a\nb\n</code></pre></li><li>b</li></ol>',
'<ol start="abc"><li>x<br>y</li><li>z</li></ol>',
'<blockquote><ul><li>a<blockquote>b<br>c</blockquote></li></ul></blockquote>',
'<ul><li>a<table><tr><td>x</td><td>y</td></tr></table></li></ul>',
'<ul><li><code>``x`</code><br><em> a </em></li></ul>',
'<ul><li>&lt;x&gt; &amp; &nbsp; &#10; &#x1f600;</li></ul>',
'<!--x--><![CDATA[test]]><iframe>x</iframe><noscript>y</noscript><script>a<b</script><style>b>a</style>',
'<ul><li>a<ul><li>b</ul></li><li>c</ul></li>',
'<blockquote><blockquote>a\n<br>b</blockquote>c</blockquote>',
];
const extra = [];
for(let i=0;i<40;i++){
 const seed=shapes[i%shapes.length];
 html=i<10?seed:i<20?'<div>'+seed+'</div>':i<30?'<ul><li>'+seed+'</li><li>tail</li></ul>':'<blockquote>'+seed+'</blockquote>';
 const value=await getURLMarkdownContent('https://example.com/page',new AbortController().signal,{fetchHook:hook,cache:new WebFetchUrlCache()});
 extra.push({name:'review-shape-'+i,html,markdown:value.content});
}
// Keep deep inline cases small enough to record literal real-TS bytes. Guards
// retain both ASCII flanks through collapseWhitespace at every span ancestor.
for (const tag of ['code', 'span']) {
 html='<p>left'+`<${tag}> `.repeat(250)+'a`b``c &amp; 😀\n'.repeat(32)+` </${tag}>`.repeat(250)+'right</p>';
 const value=await getURLMarkdownContent('https://example.com/page',new AbortController().signal,{fetchHook:hook,cache:new WebFetchUrlCache()});
 extra.push({name:`nested-${tag}-flanks-250`,html,markdown:value.content});
}
const errors = [];
for(const tag of ['ul','ol','div','blockquote','span','table','pre']){
 const pair=tag==='ul'||tag==='ol'?[`<${tag}><li>`,`</li></${tag}>`]:tag==='table'?['<table><tr><td>','</td></tr></table>']:[`<${tag}>`,`</${tag}>`];
 html=pair[0].repeat(1500)+'x'+pair[1].repeat(1500);
 try {
   await run();
   throw Error('expected overflow');
 } catch (error) {
   const e = normalizeToolError(error);
   if (e.message === 'expected overflow') throw error;
   errors.push({ tag, error: { code: e.code, message: e.message, details: e.details } });
 }
}

writeFileSync(join(resolve(process.argv[2]), 'depth-oracle.json'), JSON.stringify({
  sourceCommit: ref, sourceSha256, node: process.version, v8: process.versions.v8,
  cases, extra, errors,
}, null, 1) + '\n', { flag: 'wx' });
console.log(JSON.stringify({cases:cases.length,extra:extra.length,errors}));
