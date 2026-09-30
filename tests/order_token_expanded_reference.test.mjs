import assert from 'node:assert/strict';
import test from 'node:test';
import {retained,queries,validate} from '../scripts/capture-order-token-expanded.mjs';
const expectedOrders={
  "union same": [
    [
      1
    ]
  ],
  "union distinct same": [
    [
      1
    ]
  ],
  "union NULL": [
    [
      1
    ]
  ],
  "union mixed": [
    [
      1
    ]
  ],
  "join equivalent": [
    [
      0
    ]
  ],
  "join own": [
    [
      1
    ]
  ],
  "join both": [
    [
      2
    ]
  ],
  "join inequality": [
    [
      0
    ]
  ],
  "join left": [
    [
      0
    ]
  ],
  "fixed key": [
    [
      1
    ]
  ],
  "TOP one": [
    [
      1
    ]
  ],
  "NULL empty": [],
  "NULL TOP zero": [],
  "NULL scalar": [],
  "NULL mixed": [],
  "row number partition": [
    [
      2
    ]
  ],
  "row number empty": [
    [
      2
    ]
  ],
  "duplicate bare": [],
  "wildcard": [
    [
      2,
      1
    ]
  ],
  "qualified wildcard": [
    [
      2,
      1
    ]
  ],
  "mixed wildcard": [
    [
      2
    ]
  ],
  "joined wildcard": [
    [
      4,
      1
    ]
  ],
  "derived wildcard": [
    [
      2
    ]
  ],
  "plus two": [
    [
      1
    ]
  ],
  "multiply": [
    [
      1
    ]
  ],
  "cast": [
    [
      1
    ]
  ],
  "aggregate alias": [
    [
      2
    ]
  ],
  "sum alias": [
    [
      2
    ]
  ],
  "NULL numeric ordinal": [],
  "constant alias": [],
  "constant numeric ordinal": [],
  "NULL then column": [
    [
      1
    ]
  ],
  "column then NULL": [
    [
      1
    ]
  ],
  "two NULL keys": [],
  "literal key": [],
  "scalar folded arithmetic": [],
  "CASE constant": [
    [
      1
    ]
  ],
  "parameter key": [],
  "empty arithmetic": [
    [
      1
    ]
  ],
  "distinct wildcard": [
    [
      2,
      1
    ]
  ],
  "CTE wildcard": [
    [
      2
    ]
  ],
  "UNION wildcard": [
    [
      2,
      1
    ]
  ],
  "hidden aggregate": [
    [
      0
    ]
  ],
  "projected collate": [
    [
      1
    ]
  ],
  "hidden collate": [
    [
      0
    ]
  ],
  "row_number constant": [
    [
      1
    ]
  ],
  "prepared projected expression": [
    [
      1
    ],
    [
      1
    ],
    [
      1
    ],
    [
      1
    ]
  ],
  "prepared hidden expression": [
    [
      0
    ],
    [
      0
    ],
    [
      0
    ],
    [
      0
    ]
  ]
};

test('expanded fixture retains every request and identical independent runs',async()=>{
 const {runs}=await retained();assert.equal(queries.length,48);assert.equal(runs[0].length,98);
 for(const run of runs)for(const record of run.slice(2)){
  assert.deepEqual(record.result.events.filter(e=>e.kind==='ORDER').map(e=>e.ordinals),expectedOrders[record.name],record.name+' '+record.mode);
  assert(record.result.sets.every(set=>set.rows.every(row=>row.length===set.columns.length)),'row/descriptor alignment');
 }
});
test('expanded binding errors retain exact diagnostics and no fabricated ORDER',async()=>{
 const {runs}=await retained();
 for(const run of runs)for(const record of run.slice(2)){
  const expected={'duplicate bare':209,'literal key':408,'parameter key':1008}[record.name];
  assert.deepEqual(record.result.errors.map(e=>e.number),expected===undefined?[]:[expected],record.name);
  if(expected!==undefined)assert.equal(record.result.events.filter(e=>e.kind==='ORDER').length,0);
 }
});
test('prepared expression keys retain declaration ORDER before rows and empty execution',async()=>{
 const {runs}=await retained();
 for(const run of runs)for(const record of run.filter(r=>r.name.startsWith('prepared '))){
  assert.deepEqual(record.result.sets.map(set=>set.rows.length),[0,4,0,3],record.name);
  assert.equal(record.result.events.filter(e=>e.kind==='ORDER').length,4);
  const firstOrder=record.result.events.findIndex(e=>e.kind==='ORDER');
  const firstRow=record.result.events.findIndex(e=>e.kind==='ROW');
  assert(firstOrder>0 && firstOrder<firstRow,'prepare metadata precedes executed rows');
 }
});
test('expanded validation rejects missing requests and altered raw ORDER payloads',async()=>{
 const {runs}=await retained();
 assert.throws(()=>validate(runs[0].slice(0,-1)));
 const damaged=structuredClone(runs[0]);
 const event=damaged.find(r=>r.result.events.some(e=>e.kind==='ORDER')).result.events.find(e=>e.kind==='ORDER');
 event.hex='a9010001';assert.throws(()=>validate(damaged),/odd ORDER payload/);
});
