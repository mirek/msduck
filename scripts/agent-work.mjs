#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { acquire, verify, loadRegistry, taskId, repository, owner, claimedIds, availableTasks, applyChange, publish, restoreProtection } from './lib/agent-work.mjs';

// Only typed Git/ref/registry endpoints and project mutations are used. Never
// fetch issue/comment/review bodies, project items, notifications or search hits.
function api(path, data, missing = false, method = data ? 'POST' : 'GET') {
  try {
    const output = execFileSync('gh', ['api', '--hostname', 'github.com', path, '--method', method, ...(data ? ['--input', '-'] : [])], {
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
const rules = () => JSON.parse(readFileSync(new URL('../.github/agent-control.json', import.meta.url)));
const sleep = ms => new Promise(r => setTimeout(r, ms));
async function load() {
  // A concurrent publication briefly disables registry protection. Wait for it
  // instead of failing; nothing is read until protection is active again.
  for (let attempt = 1; ; ++attempt) {
    try { return await loadRegistry(api, rules()); }
    catch (error) {
      if (error.code !== 'PROTECTION_INACTIVE' || attempt >= 8) throw error;
      console.error('Registry publication in progress; retrying');
      await sleep(2000 * attempt);
    }
  }
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
  if (!task.projectItem) return;
  const status = state === 'done' ? 'done' : ['ready', 'backlog'].includes(state) ? 'todo' : 'progress';
  for (const [field, option] of [[p.readinessField, p.options[state]], [p.statusField, p.statuses[status]]]) {
    api('graphql', { query: 'mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,value:{singleSelectOptionId:$o}}){projectV2Item{id}}}', variables: { p:p.id, i:task.projectItem, f:field, o:option } });
  }
}
// Creates the board card before publication, because the task snapshot (and
// thus projectItem) is immutable once published. Only the issue node ID and
// author ID are requested, never issue content. Failure (e.g. a token without
// the project scope) publishes the task without a card.
function projectItem(registry, task) {
  try {
    const [login, name] = repository.split('/');
    const found = api('graphql', { query: 'query($o:String!,$n:String!,$i:Int!){repository(owner:$o,name:$n){issue(number:$i){id author{... on User{databaseId}}}}}', variables: { o: login, n: name, i: task.issue } }).data.repository.issue;
    if (found?.author?.databaseId !== owner.id) throw Error('issue author');
    const item = api('graphql', { query: 'mutation($p:ID!,$c:ID!){addProjectV2ItemById(input:{projectId:$p,contentId:$c}){item{id}}}', variables: { p: registry.project.id, c: found.id } }).data.addProjectV2ItemById.item.id;
    // The card exists now; keep its ID even if setting its fields fails.
    try { board(registry, { projectItem: item }, 'ready'); }
    catch { console.error(`Project card for ${task.id} created but its fields were not set; retry status later.`); }
    return item;
  } catch {
    console.error(`No project card for ${task.id} (owner issue and project scope required); publishing without one.`);
    return undefined;
  }
}
const usage = 'Usage: node scripts/agent-work.mjs list [--available] | claim ID | verify ID | status ID claimed|review|blocked | publish CHANGE.json [--dry-run] | protect';
async function main() {
  const [command, id, state] = process.argv.slice(2);
  if (command === 'protect') {
    // Idempotent recovery after an interrupted publication.
    await restoreProtection(api, rules().find(r => r.target === 'branch'));
    console.log(JSON.stringify({ protected: true }));
    return;
  }
  if (command === 'publish') {
    if (!id || (state !== undefined && state !== '--dry-run')) throw Error(usage);
    const change = JSON.parse(readFileSync(id, 'utf8'));
    if (typeof change.message !== 'string' || typeof change.authorization !== 'string' || change.authorization.length < 20) throw Error('Change needs a commit message and an authorization record');
    const message = `${change.message}\n\nAuthorization: ${change.authorization}`;
    if (state === '--dry-run') {
      const { registry, revision } = await load();
      const next = applyChange(registry, change, await claimedIds(api));
      console.log(JSON.stringify({ valid: true, registry: revision, tasks: next.tasks.length, added: (change.add ?? []).map(t => t.id), states: change.states ?? {} }));
      return;
    }
    const { registry } = await load();
    for (const task of change.add ?? []) if (!task.projectItem) task.projectItem = projectItem(registry, task);
    const result = await publish({ api, rules: rules(), change, message, log: m => console.error(m) });
    console.log(JSON.stringify({ published: true, registry: result.revision, previous: result.previous, added: (change.add ?? []).map(t => t.id), states: change.states ?? {} }));
    return;
  }
  if (!['list','claim','verify','status'].includes(command) || (command === 'list' ? ![undefined, '--available'].includes(id) : !id) || (command === 'status' && !['claimed','review','blocked'].includes(state))) throw Error(usage);
  const { registry, revision } = await load();
  if (command === 'list') {
    const claimed = await claimedIds(api);
    const tasks = id === '--available' ? availableTasks(registry, claimed) : registry.tasks;
    for (const task of tasks) console.log(JSON.stringify({ ...task, claim: claimed.has(task.id) ? 'claimed' : 'unclaimed', registry: revision }));
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
    const readiness = command === 'claim' ? 'claimed' : state;
    // Only the project board changes here. The registry state stays as
    // published until the owner or integrator publishes completion.
    let boardResult = task.projectItem ? 'updated' : 'no project card';
    try { board(registry, task, readiness); }
    catch { boardResult = 'failed (claim remains owned; retry status later)'; }
    console.log(JSON.stringify({ board: { readiness, result: boardResult }, registryState: task.state, note: 'registry state changes only when the integrator publishes completion' }));
  }
}
main().catch(error => { console.error(error.message); process.exitCode = 1; });
