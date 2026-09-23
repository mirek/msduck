#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { acquire, verify, loadRegistry, taskId, refFor, repository } from './lib/agent-work.mjs';

// Only typed Git/ref/registry endpoints and project mutations are used. Never
// fetch issue/comment/review bodies, project items, notifications or search hits.
function api(path, data, missing = false) {
  try {
    const output = execFileSync('gh', ['api', '--hostname', 'github.com', path, '--method', data ? 'POST' : 'GET', ...(data ? ['--input', '-'] : [])], {
      input: data ? JSON.stringify(data) : undefined, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'], maxBuffer: 2 * 1024 * 1024,
    });
    const result = JSON.parse(output);
    if (result.errors) throw Error('GraphQL failure');
    return result;
  } catch (error) {
    if (missing && /\(HTTP 404\)/.test(String(error.stderr))) return null;
    // Never echo remote error payloads or CLI stderr into the agent context.
    throw Error('GitHub API unavailable or denied; stop and check authentication/protection');
  }
}
async function load() {
  const rules = JSON.parse(readFileSync(new URL('../.github/agent-control.json', import.meta.url)));
  return loadRegistry(api, rules);
}
function receiptPath(id) {
  taskId(id);
  // Each worktree/session must keep its own ignored receipt; never copy it to a
  // different live worker or share one checkout between concurrent workers.
  const root = execFileSync('git', ['rev-parse', '--show-toplevel'], { encoding: 'utf8' }).trim();
  return resolve(root, '.msduck', 'claims', `${id}.json`);
}
function board(registry, task, state) {
  const p = registry.project;
  const status = state === 'done' ? 'done' : 'progress';
  for (const [field, option] of [[p.readinessField, p.options[state]], [p.statusField, p.statuses[status]]]) {
    api('graphql', { query: 'mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,value:{singleSelectOptionId:$o}}){projectV2Item{id}}}', variables: { p:p.id, i:task.projectItem, f:field, o:option } });
  }
}
async function main() {
  const [command, id, state] = process.argv.slice(2);
  if (!['list','claim','verify','status'].includes(command) || (command !== 'list' && !id) || (command === 'status' && !['claimed','review','blocked'].includes(state))) throw Error('Usage: node scripts/agent-work.mjs list | claim ID | verify ID | status ID claimed|review|blocked');
  const { registry, revision } = await load();
  if (command === 'list') {
    for (const task of registry.tasks) {
      const claim = api(`repos/${repository}/git/ref/${refFor(task.id).slice(5)}`, undefined, true);
      console.log(JSON.stringify({ ...task, claim: claim ? 'claimed' : 'unclaimed', registry: revision }));
    }
    return;
  }
  let task;
  if (command === 'claim') {
    const path = receiptPath(id);
    mkdirSync(resolve(path, '..'), { recursive: true });
    const result = await acquire({ api, registry, revision, id, nonce: randomUUID(), saveReceipt: receipt => writeFileSync(path, JSON.stringify(receipt, null, 2) + '\n', { flag: 'wx', mode: 0o600 }) });
    task = result.task;
    console.log(JSON.stringify({ acquired: true, task, receipt: path, sha: result.receipt.sha }));
  } else {
    task = await verify({ api, registry, id, receipt: JSON.parse(readFileSync(receiptPath(id), 'utf8')) });
    console.log(JSON.stringify({ owned: true, task }));
  }
  if (command !== 'verify') {
    try { board(registry, task, command === 'claim' ? 'claimed' : state); }
    catch { console.error('Claim remains owned; project update failed. Retry status later.'); }
  }
}
main().catch(error => { console.error(error.message); process.exitCode = 1; });
