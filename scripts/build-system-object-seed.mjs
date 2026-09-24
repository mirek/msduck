#!/usr/bin/env node
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';

const fixture = JSON.parse(readFileSync(new URL('../reference/all-objects.json', import.meta.url), 'utf8'));
const output = new URL('../src/object_catalog/system_objects.json', import.meta.url);
const captureColumns = [
  'name', 'object_id', 'principal_id', 'schema_id', 'parent_object_id',
  'type', 'type_desc', 'create_date', 'modify_date', 'is_ms_shipped',
  'is_published', 'is_schema_published', 'schema_name', 'parent_name',
  'parent_schema', 'in_objects', 'in_system_objects',
];
const seedColumns = [
  'name', 'object_id', 'principal_id', 'schema_id', 'parent_object_id',
  'type', 'type_desc', 'create_date', 'modify_date', 'is_ms_shipped',
  'is_published', 'is_schema_published', 'in_objects', 'in_system_objects',
  'dynamic_clock',
];

function catalog(run) {
  const observation = run.find(({ name }) => name === 'fresh full catalog');
  assert(observation, 'missing fresh full catalog observation');
  assert.deepEqual(observation.result.errors, []);
  assert.deepEqual(observation.result.sets[0].columns.map(({ name }) => name), captureColumns);
  const rows = observation.result.sets[0].rows.map(values => {
    assert.equal(values.length, captureColumns.length);
    return Object.fromEntries(captureColumns.map((name, index) => [name, values[index]]));
  });
  const ids = new Set(rows.map(({ object_id }) => object_id));
  assert.equal(ids.size, rows.length, 'object IDs must be unique');
  for (const row of rows) {
    assert.notEqual(row.in_objects, row.in_system_objects, `${row.name}: membership must be disjoint`);
    assert(row.parent_object_id === 0 || ids.has(row.parent_object_id), `${row.name}: missing parent`);
    assert.equal(row.is_ms_shipped, true, `${row.name}: unexpected non-shipped baseline row`);
  }
  return new Map(rows.map(row => [row.object_id, row]));
}

assert.equal(fixture.runs.length, 2, 'expected independent retained runs');
const first = catalog(fixture.runs[0]);
const second = catalog(fixture.runs[1]);
assert.deepEqual([...first.keys()].sort((a, b) => a - b), [...second.keys()].sort((a, b) => a - b));
const rows = [...first.values()].sort((a, b) => a.object_id - b.object_id).map(row => {
  const again = second.get(row.object_id);
  const { create_date, modify_date, ...stable } = row;
  const { create_date: nextCreated, modify_date: nextModified, ...nextStable } = again;
  assert.deepEqual(nextStable, stable, `${row.name}: unstable catalog identity or membership`);
  const created = create_date?.value;
  const modified = modify_date?.value;
  const nextCreatedValue = nextCreated?.value;
  const nextModifiedValue = nextModified?.value;
  assert.equal(create_date?.kind, 'date');
  assert.equal(modify_date?.kind, 'date');
  assert.equal(nextCreated?.kind, 'date');
  assert.equal(nextModified?.kind, 'date');
  if (created === nextCreatedValue && modified === nextModifiedValue) {
    return { ...stable, create_date: created, modify_date: modified, dynamic_clock: false };
  }
  assert.equal(row.name, 'wpr_bucket_table', 'unexpected clock-dependent catalog row');
  assert.equal(row.schema_name, 'sys');
  assert.equal(row.type, 'IT');
  assert.equal(created, modified);
  assert.equal(nextCreatedValue, nextModifiedValue);
  return { ...stable, create_date: null, modify_date: null, dynamic_clock: true };
});

const seed = {
  source_image: fixture.image,
  source_observation: 'fresh full catalog',
  columns: seedColumns,
  rows: rows.map(row => seedColumns.map(name => row[name])),
};
writeFileSync(output, JSON.stringify(seed) + '\n');
const objects = rows.filter(row => row.in_objects).length;
const system = rows.filter(row => row.in_system_objects).length;
process.stdout.write(`${rows.length} built-in rows (${objects} objects, ${system} system_objects); ${rows.filter(row => row.dynamic_clock).length} clock-dependent\n`);
