import { createHash } from 'node:crypto';

export const repository = 'mirek/msduck';
export const owner = { login: 'mirek', id: 8561 };
export const refFor = id => `refs/tags/agent-claims/${id}`;
const idPattern = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
export function taskId(id) {
  if (typeof id !== 'string' || !idPattern.test(id) || id.length > 100) throw Error('Invalid task ID');
  return id;
}
export function digest(task) {
  return createHash('sha256').update(JSON.stringify(task)).digest('hex');
}
export function validateRegistry(value) {
  if (value?.version !== 1 || value.repository !== repository || value.owner?.id !== owner.id || value.owner?.login !== owner.login || !Array.isArray(value.tasks)) throw Error('Untrusted task registry');
  const ids = new Set();
  for (const task of value.tasks) {
    taskId(task.id);
    if (ids.has(task.id)) throw Error('Duplicate task ID');
    ids.add(task.id);
    if (!['ready', 'backlog', 'blocked', 'review', 'done'].includes(task.state) || typeof task.title !== 'string' || !Number.isSafeInteger(task.issue) || task.issue < 1 || !Array.isArray(task.scope) || task.scope.length === 0 || !Array.isArray(task.acceptance) || task.acceptance.length === 0) throw Error('Incomplete task');
    for (const path of task.scope) {
      if (typeof path !== 'string' || path.startsWith('/') || path.includes('..') || !/^[a-zA-Z0-9_./-]+$/.test(path)) throw Error('Invalid task scope');
    }
    if (task.dependencies !== undefined && (!Array.isArray(task.dependencies) || task.dependencies.some(id => typeof id !== 'string'))) throw Error('Invalid dependencies');
  }
  for (const task of value.tasks) {
    for (const dep of task.dependencies ?? []) if (!ids.has(dep) || dep === task.id) throw Error('Unknown or self dependency');
  }
  // File ownership is deliberately conservative. Shared files require serial tasks.
  const ready = value.tasks.filter(t => t.state === 'ready');
  for (let i = 0; i < ready.length; ++i) for (const other of ready.slice(i + 1)) {
    if (ready[i].scope.some(a => other.scope.some(b => a === b || (a.endsWith('/') && b.startsWith(a)) || (b.endsWith('/') && a.startsWith(b))))) throw Error('Overlapping ready task scopes');
  }
  return value;
}
export function readyTask(registry, id) {
  taskId(id);
  const task = registry.tasks.find(t => t.id === id);
  if (!task || task.state !== 'ready') throw Error('Task is not owner-approved and ready');
  if ((task.dependencies ?? []).some(id => registry.tasks.find(t => t.id === id)?.state !== 'done')) throw Error('Task dependencies are unfinished');
  return task;
}

// A claim is a create-only ref pointing to a unique receipt commit. Never update,
// delete or expire it. The create operation, not the preliminary read, arbitrates.
export async function acquire({ api, registry, revision, id, nonce, saveReceipt }) {
  const task = readyTask(registry, id);
  const user = await api('user');
  if (user.id !== owner.id || user.login !== owner.login) throw Error('Only the repository owner may authorize a worker');
  const ref = refFor(id);
  if (await api(`repos/${repository}/git/ref/${ref.slice(5)}`, undefined, true)) throw Error('Task already claimed; choose another task');
  const base = await api(`repos/${repository}/git/commits/${revision}`);
  const payload = { version: 1, task: id, nonce, registry: revision, taskDigest: digest(task) };
  const commit = await api(`repos/${repository}/git/commits`, { message: JSON.stringify(payload), tree: base.tree.sha, parents: [revision] });
  const receipt = { ...payload, sha: commit.sha, ref };
  // Persist before POST so a lost response is recoverable without a second claim.
  await saveReceipt(receipt);
  try {
    await api(`repos/${repository}/git/refs`, { ref, sha: commit.sha });
  } catch {
    // An HTTP error is not assumed to mean contention. Read back the exact ref.
    const current = await api(`repos/${repository}/git/ref/${ref.slice(5)}`, undefined, true);
    if (current?.object?.sha !== commit.sha) throw Error('Claim not acquired (conflict or API failure); do not begin work');
  }
  const current = await api(`repos/${repository}/git/ref/${ref.slice(5)}`);
  if (current.object.sha !== commit.sha) throw Error('Claim verification failed; do not begin work');
  return { task, receipt };
}
export async function verify({ api, registry, id, receipt }) {
  taskId(id);
  const user = await api('user');
  if (user.id !== owner.id || user.login !== owner.login) throw Error('Only the repository owner may authorize a worker');
  const task = registry.tasks.find(t => t.id === id);
  if (!task || !['ready', 'review'].includes(task.state) || receipt.task !== id || receipt.taskDigest !== digest(task) || receipt.ref !== refFor(id)) throw Error('Task changed, revoked, or receipt does not match; stop work');
  const current = await api(`repos/${repository}/git/ref/${refFor(id).slice(5)}`);
  if (current.object.sha !== receipt.sha) throw Error('This session does not own the claim');
  return task;
}

function checkRuleset(actual, expected) {
  if (!(['active', 'disabled'].includes(actual.enforcement) && actual.target === expected.target && actual.bypass_actors?.length === 0 && actual.conditions?.ref_name?.include?.includes(expected.pattern) && actual.conditions.ref_name.exclude.length === 0 && ['update','deletion'].every(type => actual.rules.some(r => r.type === type)))) throw Error('Coordination protection changed; stop and ask the owner');
}
export async function loadRegistry(api, rules) {
  if (rules.length !== 2 || !rules.some(r => r.target === 'tag' && r.pattern === 'refs/tags/agent-claims/**') || !rules.some(r => r.target === 'branch' && r.pattern === 'refs/heads/agent-control')) throw Error('Missing coordination protection configuration');
  for (const expected of rules) {
    const actual = await api(`repos/${repository}/rulesets/${expected.id}`);
    checkRuleset(actual, expected);
    if (actual.enforcement !== 'active') {
      // Publishers disable only the registry rule, briefly. Callers may retry;
      // registry content is never read while protection is inactive.
      if (expected.target === 'branch') throw Object.assign(Error('Registry publication in progress (protection inactive); retry shortly, or run `node scripts/agent-work.mjs protect` if it persists'), { code: 'PROTECTION_INACTIVE' });
      throw Error('Coordination protection changed; stop and ask the owner');
    }
  }
  const ref = await api(`repos/${repository}/git/ref/heads/agent-control`);
  const revision = ref.object.sha;
  if (!/^[a-f0-9]{40}$/.test(revision)) throw Error('Invalid registry revision');
  const file = await api(`repos/${repository}/contents/work.json?ref=${revision}`);
  if (file.encoding !== 'base64') throw Error('Unsupported registry representation');
  const registry = validateRegistry(JSON.parse(Buffer.from(file.content, 'base64').toString('utf8')));
  return { registry, revision };
}

// Claim tags for every task in one request instead of one lookup per task.
export async function claimedIds(api) {
  const refs = await api(`repos/${repository}/git/matching-refs/tags/agent-claims/`);
  if (!Array.isArray(refs)) throw Error('Unexpected claim listing');
  return new Set(refs.map(r => r.ref.slice(refFor('').length)));
}
export function availableTasks(registry, claimed) {
  const done = new Set(registry.tasks.filter(t => t.state === 'done').map(t => t.id));
  return registry.tasks.filter(t => t.state === 'ready' && !claimed.has(t.id) && (t.dependencies ?? []).every(id => done.has(id)));
}

const states = ['ready', 'backlog', 'blocked', 'review', 'done'];
// A publication adds new tasks and changes states of existing tasks. Existing
// task definitions are otherwise immutable, because receipts bind their digest,
// and IDs are never removed or reused (claim tags are permanent).
export function applyChange(registry, change, claimed) {
  const next = structuredClone(registry);
  const known = new Set(next.tasks.map(t => t.id));
  for (const [id, state] of Object.entries(change.states ?? {})) {
    const task = next.tasks.find(t => t.id === id);
    if (!task) throw Error(`Unknown task ${id}`);
    if (!states.includes(state)) throw Error(`Invalid state for ${id}`);
    task.state = state;
  }
  for (const task of change.add ?? []) {
    taskId(task.id);
    if (known.has(task.id) || claimed.has(task.id)) throw Error(`Task ID ${task.id} already exists or was claimed; choose a new ID`);
    if (typeof task.authorization !== 'string' || task.authorization.length < 20) throw Error(`Task ${task.id} needs an authorization record`);
    known.add(task.id);
    next.tasks.push(task);
  }
  validateRegistry(next);
  return next;
}

// Publishes by fast-forwarding agent-control from the revision it validated.
// Concurrent publishers race on the ref update; the loser reloads and reapplies.
// The registry rule is disabled only around the update and always re-enabled.
export async function publish({ api, rules, change, message, attempts = 6, sleep = ms => new Promise(r => setTimeout(r, ms)), log = () => {} }) {
  const rule = rules.find(r => r.target === 'branch');
  for (let attempt = 1; attempt <= attempts; ++attempt) {
    let loaded;
    try { loaded = await loadRegistry(api, rules); }
    catch (error) {
      if (error.code !== 'PROTECTION_INACTIVE') throw error;
      log('registry protection inactive; waiting for concurrent publication');
      await sleep(2000 * attempt);
      continue;
    }
    const { registry, revision } = loaded;
    const next = applyChange(registry, change, await claimedIds(api));
    const content = JSON.stringify(next, null, 2) + '\n';
    const base = await api(`repos/${repository}/git/commits/${revision}`);
    const blob = await api(`repos/${repository}/git/blobs`, { content, encoding: 'utf-8' });
    const tree = await api(`repos/${repository}/git/trees`, { base_tree: base.tree.sha, tree: [{ path: 'work.json', mode: '100644', type: 'blob', sha: blob.sha }] });
    const commit = await api(`repos/${repository}/git/commits`, { message, tree: tree.sha, parents: [revision] });
    let updated = false;
    try {
      await api(`repos/${repository}/rulesets/${rule.id}`, { enforcement: 'disabled' }, false, 'PUT');
      try { await api(`repos/${repository}/git/refs/heads/agent-control`, { sha: commit.sha, force: false }, false, 'PATCH'); updated = true; }
      catch { /* Not a fast-forward, re-protected by another publisher, or lost response; read back below. */ }
    } finally {
      await restoreProtection(api, rule);
    }
    const current = await api(`repos/${repository}/git/ref/heads/agent-control`);
    if (current.object.sha === commit.sha) return { revision: commit.sha, registry: next, previous: revision };
    if (updated) throw Error('Registry ref changed after publication; inspect agent-control history');
    log(`publication attempt ${attempt} lost a race; reloading`);
    await sleep(1000 * attempt + Math.floor(Math.random() * 1000));
  }
  throw Error('Registry publication did not succeed; retry later');
}
export async function restoreProtection(api, rule) {
  for (let i = 0; i < 5; ++i) {
    try {
      await api(`repos/${repository}/rulesets/${rule.id}`, { enforcement: 'active' }, false, 'PUT');
      const actual = await api(`repos/${repository}/rulesets/${rule.id}`);
      checkRuleset(actual, rule);
      if (actual.enforcement === 'active') return;
    } catch { // Retry; never surface remote error payloads.
    }
  }
  throw Error('Registry protection could not be restored; run `node scripts/agent-work.mjs protect` now');
}
