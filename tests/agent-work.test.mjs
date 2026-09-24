import test from 'node:test';
import assert from 'node:assert/strict';
import { acquire, verify, loadRegistry, validateRegistry, readyTask, refFor, repository } from '../scripts/lib/agent-work.mjs';

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
