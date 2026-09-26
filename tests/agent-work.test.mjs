import test from 'node:test';
import assert from 'node:assert/strict';
import { acquire, verify, loadRegistry, validateRegistry, readyTask, refFor, repository, applyChange, availableTasks, claimedIds, publish, restoreProtection } from '../scripts/lib/agent-work.mjs';

function fixture() {
  return validateRegistry({ version:1, repository, owner:{login:'mirek',id:8561}, tasks:[{id:'example-v1',title:'Bounded task',state:'ready',issue:1,scope:['docs/example.md'],acceptance:['Evidence exists']}] });
}
function server({ lostResponse=false, denied=false, user={login:'mirek',id:8561} }={}) {
  const refs = new Map(); const calls=[]; let next=0;
  const api=async (path,data,missing=false) => {
    calls.push({path,data});
    // Yield so competing claims both see the unclaimed initial state.
    await new Promise(resolve => setImmediate(resolve));
    if(path==='user')return user;
    if(path===`repos/${repository}/git/commits/base`)return {tree:{sha:'tree'}};
    if(path===`repos/${repository}/git/commits`)return {sha:`unique-${++next}`};
    if(path===`repos/${repository}/git/refs`) {
      if(denied || refs.has(data.ref))throw Error('denied or duplicate');
      refs.set(data.ref,data.sha);
      if(lostResponse)throw Error('response lost after successful write');
      return {object:{sha:data.sha}};
    }
    const prefix=`repos/${repository}/git/ref/`;
    if(path.startsWith(prefix)) {
      const sha=refs.get('refs/'+path.slice(prefix.length));
      if(sha)return {object:{sha}};
      if(missing)return null;
      throw Error('missing');
    }
    throw Error('Unexpected endpoint: '+path);
  };
  return {api,refs,calls};
}
function claim(server,registry=fixture(),extra={}) {
  let receipt;
  const result=acquire({api:server.api,registry,revision:'base',id:'example-v1',nonce:crypto.randomUUID(),saveReceipt:r=>{receipt=r;},...extra});
  return {result,receipt:()=>receipt};
}

test('concurrent contenders have exactly one winner and cannot adopt the winner receipt',async()=>{
  const s=server(); const contenders=Array.from({length:16},()=>claim(s));
  const results=await Promise.allSettled(contenders.map(c=>c.result));
  assert.equal(results.filter(r=>r.status==='fulfilled').length,1);
  assert.equal(s.refs.size,1);
  for(let i=0;i<results.length;i++) {
    const args={api:s.api,registry:fixture(),id:'example-v1',receipt:contenders[i].receipt()};
    if(results[i].status==='fulfilled') await verify(args);
    else await assert.rejects(verify(args),/does not own/);
  }
  assert(s.calls.every(c=>c.path==='user'||c.path.startsWith(`repos/${repository}/git/`)));
  assert(s.calls.filter(c=>c.data?.ref).every(c=>c.path.endsWith('/git/refs')));
});
test('lost response is recovered only when exact unique commit owns ref',async()=>{
  const s=server({lostResponse:true}); const c=claim(s);
  await c.result;
  assert.equal(s.refs.get(refFor('example-v1')),c.receipt().sha);
  await verify({api:s.api,registry:fixture(),id:'example-v1',receipt:c.receipt()});
});
test('API refusal never grants a claim',async()=>{
  const s=server({denied:true}); await assert.rejects(claim(s).result,/not acquired/); assert.equal(s.refs.size,0);
});
test('receipt must be durable before remote ref creation',async()=>{
  const s=server(); await assert.rejects(claim(s,fixture(),{saveReceipt:()=>{throw Error('disk full');}}).result,/disk full/);
  assert.equal(s.refs.size,0);
  assert.equal(s.calls.filter(c=>c.path.endsWith('/git/refs')).length,0);
});
test('wrong identity cannot create a commit or claim',async()=>{
  for(const user of [{login:'mirek',id:99},{login:'other',id:8561}]) {
    const s=server({user}); await assert.rejects(claim(s).result,/Only the repository owner/); assert.deepEqual(s.calls.map(c=>c.path),['user']);
  }
});
test('unready, unknown and dependency-blocked tasks make no API requests',async()=>{
  for(const state of ['backlog','blocked','done','review']) {
    const r=fixture();r.tasks[0].state=state;const s=server();await assert.rejects(claim(s,r).result,/not owner-approved/);assert.equal(s.calls.length,0);
  }
  const r=fixture(); r.tasks[0].dependencies=['missing'];assert.throws(()=>readyTask(r,'example-v1'),/dependencies/);
  assert.throws(()=>readyTask(r,'other-v1'),/not owner-approved/);
});
test('a changed or revoked snapshot invalidates an existing receipt',async()=>{
  const s=server(); const c=claim(s); await c.result;
  for(const change of [t=>t.state='blocked',t=>t.acceptance.push('Changed acceptance'),t=>t.scope.push('src/engine.rs')]) {
    const r=fixture();change(r.tasks[0]); await assert.rejects(verify({api:s.api,registry:r,id:'example-v1',receipt:c.receipt()}),/changed, revoked/);
  }
});
test('registry rejects impersonation, traversal, duplicate IDs and overlapping scopes',()=>{
  for(const change of [r=>r.owner.id=99,r=>r.repository='other/repo',r=>r.tasks[0].id='../bad',r=>r.tasks[0].scope=['../secret'],r=>r.tasks.push({...r.tasks[0]}),r=>r.tasks.push({...r.tasks[0],id:'other-v1',scope:['docs/']}),r=>r.tasks[0].dependencies=['missing']]) {
    const r=fixture();change(r);assert.throws(()=>validateRegistry(r));
  }
  const r=fixture();r.tasks.push({...r.tasks[0],id:'other-v1',scope:['docs/independent.md']});assert.equal(validateRegistry(r).tasks.length,2);
});

const rules = [
  {id:1,target:'tag',pattern:'refs/tags/agent-claims/**'},
  {id:2,target:'branch',pattern:'refs/heads/agent-control'},
];
function registryServer(change = () => {}) {
  const calls = [];
  const revision = 'a'.repeat(40);
  const api = async path => {
    calls.push(path);
    const expected = rules.find(r => path === `repos/${repository}/rulesets/${r.id}`);
    if (expected) {
      const rule = {enforcement:'active',target:expected.target,bypass_actors:[],conditions:{ref_name:{include:[expected.pattern],exclude:[]}},rules:[{type:'update'},{type:'deletion'}]};
      change(rule); return rule;
    }
    if (path === `repos/${repository}/git/ref/heads/agent-control`) return {object:{sha:revision}};
    if (path === `repos/${repository}/contents/work.json?ref=${revision}`) return {encoding:'base64',content:Buffer.from(JSON.stringify(fixture())).toString('base64')};
    throw Error('Forbidden content endpoint');
  };
  return {api,calls};
}
test('discovery fetches only protection metadata and a revision-pinned approved registry', async () => {
  const s=registryServer(); const result=await loadRegistry(s.api,rules);
  assert.equal(result.registry.tasks[0].id,'example-v1');assert.equal(s.calls.length,4);
  assert(s.calls.at(-1).endsWith('?ref='+'a'.repeat(40)));
});
test('disabled, bypassable, incomplete or excluding rules stop before any content fetch',async()=>{
  for (const change of [r=>r.enforcement='disabled',r=>r.bypass_actors=[{actor_id:5}],r=>r.rules.pop(),r=>r.conditions.ref_name.exclude=['refs/tags/agent-claims/example-v1'],r=>r.conditions.ref_name.include=[]]) {
    const s=registryServer(change);await assert.rejects(loadRegistry(s.api,rules),/protection changed/);assert.equal(s.calls.length,1);
  }
  const s=registryServer();await assert.rejects(loadRegistry(s.api,[]),/Missing/);assert.equal(s.calls.length,0);
});

test('inactive registry protection is a retryable publication state and fetches no content',async()=>{
  const s=registryServer(r=>{ if(r.target==='branch') r.enforcement='disabled'; });
  await assert.rejects(loadRegistry(s.api,rules),e=>e.code==='PROTECTION_INACTIVE');
  assert(s.calls.every(p=>p.includes('/rulesets/')));
});

const authorization='Direct owner instruction recorded for this test task';
const task=(id,scope=[`docs/${id}.md`])=>({id,title:'Bounded task',state:'ready',authorization,issue:2,scope,acceptance:['Evidence exists']});
test('changes add tasks and set states but never reuse IDs or skip validation',()=>{
  const r=fixture(); const none=new Set();
  const next=applyChange(r,{add:[task('new-v1')],states:{'example-v1':'done'}},none);
  assert.deepEqual(next.tasks.map(t=>[t.id,t.state]),[['example-v1','done'],['new-v1','ready']]);
  assert.equal(r.tasks.length,1);
  for (const [change,claimed,pattern] of [
    [{add:[task('example-v1')]},none,/already exists/],
    [{add:[task('old-v1')]},new Set(['old-v1']),/already exists or was claimed/],
    [{add:[{...task('new-v1'),authorization:undefined}]},none,/authorization/],
    [{add:[task('new-v1',['docs/example.md'])]},none,/Overlapping/],
    [{add:[{...task('new-v1'),dependencies:['missing-v1']}]},none,/Unknown/],
    [{states:{'missing-v1':'done'}},none,/Unknown task/],
    [{states:{'example-v1':'finished'}},none,/Invalid state/],
  ]) assert.throws(()=>applyChange(r,change,claimed),pattern);
});
test('available tasks are ready, unclaimed and have completed dependencies',()=>{
  const r=applyChange(fixture(),{add:[task('free-v1'),{...task('waiting-v1'),dependencies:['example-v1']},task('taken-v1')]},new Set());
  assert.deepEqual(availableTasks(r,new Set(['taken-v1'])).map(t=>t.id),['example-v1','free-v1']);
  r.tasks[0].state='done';
  assert.deepEqual(availableTasks(r,new Set(['taken-v1'])).map(t=>t.id),['free-v1','waiting-v1']);
});

// Simulated GitHub: rulesets, git objects, fast-forward-only ref updates that
// the active registry rule refuses, and create-only claim tags.
function github({failPatch=0,failRestore=0}={}) {
  const objects=new Map(); let next=0; const calls=[];
  const put=(value)=>{const sha=(++next).toString(16).padStart(40,'0');objects.set(sha,value);return sha;};
  const blob=put({content:JSON.stringify(fixture())});
  const head={sha:put({tree:put({entries:{'work.json':blob}}),parents:[]})};
  const enforcement=new Map(rules.map(r=>[r.id,'active']));
  const claims=['refs/tags/agent-claims/example-v1'];
  let activePublishers=0, maxDisabled=0;
  const api=async(path,data,missing=false,method=data?'POST':'GET')=>{
    calls.push({path,method});
    await new Promise(resolve=>setImmediate(resolve));
    const p=path.replace(`repos/${repository}/`,'');
    const rule=rules.find(r=>p===`rulesets/${r.id}`);
    if(rule&&method==='GET') return {enforcement:enforcement.get(rule.id),target:rule.target,bypass_actors:[],conditions:{ref_name:{include:[rule.pattern],exclude:[]}},rules:[{type:'update'},{type:'deletion'}]};
    if(rule&&method==='PUT') {
      if(data.enforcement==='active'&&failRestore-->0) throw Error('transient');
      enforcement.set(rule.id,data.enforcement);
      activePublishers+=data.enforcement==='disabled'?1:0; maxDisabled=Math.max(maxDisabled,activePublishers);
      return {};
    }
    if(p==='git/ref/heads/agent-control') return {object:{sha:head.sha}};
    if(p==='git/matching-refs/tags/agent-claims/') return claims.map(ref=>({ref}));
    if(p.startsWith('contents/work.json?ref=')) {
      const commit=objects.get(p.split('=')[1]);
      return {encoding:'base64',content:Buffer.from(objects.get(objects.get(commit.tree).entries['work.json']).content).toString('base64')};
    }
    if(p.startsWith('git/commits/')) return {tree:{sha:objects.get(p.slice(12)).tree}};
    if(p==='git/blobs') return {sha:put({content:data.content})};
    if(p==='git/trees') return {sha:put({entries:{...objects.get(data.base_tree).entries,[data.tree[0].path]:data.tree[0].sha}})};
    if(p==='git/commits') return {sha:put({tree:data.tree,parents:data.parents})};
    if(p==='git/refs/heads/agent-control'&&method==='PATCH') {
      if(failPatch-->0) throw Error('lost');
      if(enforcement.get(2)==='active') throw Error('rule violation');
      if(data.force!==false||objects.get(data.sha).parents[0]!==head.sha) throw Error('not a fast-forward');
      head.sha=data.sha; return {object:{sha:data.sha}};
    }
    throw Error('Unexpected endpoint: '+method+' '+path);
  };
  const registry=()=>JSON.parse(objects.get(objects.get(objects.get(head.sha).tree).entries['work.json']).content);
  return {api,calls,enforcement,registry,history:()=>{const out=[];for(let s=head.sha;s;s=objects.get(s).parents[0])out.push(s);return out;},maxDisabled:()=>maxDisabled};
}
const fast={sleep:()=>new Promise(resolve=>setImmediate(resolve))};
test('concurrent publishers serialize as fast-forwards and always restore protection',async()=>{
  const g=github();
  const results=await Promise.allSettled(Array.from({length:6},(_,i)=>publish({api:g.api,rules,change:{add:[task(`parallel-${i}-v1`)]},message:`add ${i}`,attempts:40,...fast})));
  assert.deepEqual(results.map(r=>r.status),Array(6).fill('fulfilled'));
  assert.deepEqual(g.registry().tasks.map(t=>t.id).sort(),['example-v1',...Array.from({length:6},(_,i)=>`parallel-${i}-v1`)].sort());
  assert.equal(g.history().length,7);
  assert.equal(g.enforcement.get(2),'active'); assert.equal(g.enforcement.get(1),'active');
  assert(g.calls.every(c=>!/rulesets\/1$/.test(c.path)||c.method==='GET'),'claim protection is never modified');
  assert(g.calls.every(c=>!/issues|comments|pulls|search|notifications|projects/.test(c.path)));
});
test('a lost update response is resolved by reading the ref back',async()=>{
  const g=github({failPatch:1});
  await assert.rejects(publish({api:g.api,rules,change:{add:[task('new-v1')]},message:'m',attempts:1,...fast}),/did not succeed/);
  assert.equal(g.enforcement.get(2),'active');
  const r=await publish({api:g.api,rules,change:{add:[task('new-v1')]},message:'m',...fast});
  assert.equal(r.registry.tasks.length,2); assert.equal(g.history().length,2);
});
test('conflicting publication fails validation after reload instead of overwriting',async()=>{
  const g=github();
  const results=await Promise.allSettled([1,2].map(()=>publish({api:g.api,rules,change:{add:[task('same-v1')]},message:'m',attempts:10,...fast})));
  assert.equal(results.filter(r=>r.status==='fulfilled').length,1);
  assert.match(results.find(r=>r.status==='rejected').reason.message,/already exists/);
  assert.equal(g.registry().tasks.length,2); assert.equal(g.enforcement.get(2),'active');
});
test('protection restore retries and reports failure for manual recovery',async()=>{
  const g=github({failRestore:2}); g.enforcement.set(2,'disabled');
  await restoreProtection(g.api,rules[1]); assert.equal(g.enforcement.get(2),'active');
  const h=github({failRestore:99}); h.enforcement.set(2,'disabled');
  await assert.rejects(restoreProtection(h.api,rules[1]),/protect/);
});
test('publication waits while another publisher holds protection inactive',async()=>{
  const g=github(); g.enforcement.set(2,'disabled');
  let waits=0; const sleep=async()=>{ if(++waits===2) g.enforcement.set(2,'active'); await fast.sleep(); };
  await publish({api:g.api,rules,change:{states:{'example-v1':'done'}},message:'m',sleep});
  assert.equal(g.registry().tasks[0].state,'done'); assert(waits>=2);
});
test('claim listing uses one request',async()=>{
  const g=github(); assert.deepEqual([...await claimedIds(g.api)],['example-v1']); assert.equal(g.calls.length,1);
});
