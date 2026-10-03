#!/usr/bin/env node
// SQL Server evidence for DML triggers (issue #721): result rows, ERROR and
// INFO tokens and raw DONE tokens for trigger definitions, firing with
// multirow inserted/deleted images, UPDATE()/COLUMNS_UPDATED(), nesting,
// INSTEAD OF, DISABLE/ENABLE and error/ROLLBACK propagation, and MERGE
// firing (issue #852).
//
// Every case runs in its own database of one fresh pinned container, batch by
// batch through tedious's SQL batch path. tests/compat/triggers.test.mjs
// replays the retained cases against msduck.
//
//   node scripts/capture-gaps-triggers.mjs [--write-fixture] [output]
import { createHash } from 'node:crypto'
import { mkdir, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import { Request } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { referenceImage, withReferenceContainer } from './lib/reference-container.mjs'
import { connect, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/gaps-triggers.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => arg !== '--write-fixture')
if (paths.length > 1) throw Error('usage: capture-gaps-triggers.mjs [--write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/gaps-triggers-reference/capture.json')

// DONE bodies (TDS 7.2+: USHORT status, USHORT CurCmd, ULONGLONG row count),
// recorded verbatim before tedious parses them.
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneTypes = new Map([[0xFD, 'done'], [0xFE, 'doneProc'], [0xFF, 'doneInProc']])
const sinks = new WeakMap()
const readToken = StreamParser.prototype.readToken
StreamParser.prototype.readToken = function (type) {
  return doneTypes.has(type) ? recordDone(this, type) : readToken.call(this, type)
}
function recordDone(parser, type) {
  const at = parser.position
  if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordDone(parser, type))
  sinks.get(parser.options)?.push({
    kind: doneTypes.get(type),
    status: parser.buffer.readUInt16LE(at),
    curCmd: parser.buffer.readUInt16LE(at + 2),
    rowCount: parser.buffer.readBigUInt64LE(at + 4).toString(),
  })
  return readToken.call(parser, type)
}

// The cases. Each runs in a fresh database; every string is one batch.
export const cases = [
  {
    name: 'after triggers: images, UPDATE(), COLUMNS_UPDATED(), @@ROWCOUNT and nesting levels',
    batches: [
      `CREATE TABLE t3(id INT PRIMARY KEY, v INT, w INT)
CREATE TABLE log3(seq INT IDENTITY, msg VARCHAR(200), cu VARBINARY(10))`,
      `CREATE TRIGGER t3_iud ON t3 AFTER INSERT, UPDATE, DELETE AS
  DECLARE @rc INT = @@ROWCOUNT;
  INSERT log3(msg, cu) SELECT 'rc=' + CAST(@rc AS VARCHAR) + ' tc=' + CAST(@@TRANCOUNT AS VARCHAR)
    + ' i=' + CAST((SELECT COUNT(*) FROM inserted) AS VARCHAR) + ' d=' + CAST((SELECT COUNT(*) FROM deleted) AS VARCHAR)
    + ' uv=' + CAST(CASE WHEN UPDATE(v) THEN 1 ELSE 0 END AS VARCHAR) + ' uw=' + CAST(CASE WHEN UPDATE(w) THEN 1 ELSE 0 END AS VARCHAR)
    + ' nest=' + CAST(TRIGGER_NESTLEVEL() AS VARCHAR) + ' nl=' + CAST(@@NESTLEVEL AS VARCHAR), COLUMNS_UPDATED();`,
      'INSERT t3(id, v) VALUES (1, 1), (2, 2); SELECT @@ROWCOUNT AS rc',
      'INSERT t3(id, v) SELECT 9, 9 WHERE 1 = 0; SELECT @@ROWCOUNT AS rc',
      'UPDATE t3 SET w = 5; SELECT @@ROWCOUNT AS rc',
      'UPDATE t3 SET v = v, w = w WHERE id = 1',
      'UPDATE t3 SET v = 1 WHERE id = 42',
      'DELETE t3 WHERE id = 2',
      'BEGIN TRAN; INSERT t3 VALUES (7, 7, 7); COMMIT',
      'SELECT msg, cu FROM log3 ORDER BY seq',
    ],
  },
  {
    name: 'after triggers: key updates, joined and CTE statements',
    batches: [
      `CREATE TABLE k(id INT PRIMARY KEY, code VARCHAR(10) UNIQUE, n INT)
CREATE TABLE src(id INT, code VARCHAR(10))
CREATE TABLE klog(op CHAR(1), id INT, code VARCHAR(10), n INT)
INSERT k VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30)
INSERT src VALUES (2, 'zz'), (3, 'yy')`,
      `CREATE TRIGGER k_log ON k AFTER UPDATE, DELETE AS
  SET NOCOUNT ON;
  INSERT klog SELECT 'D', id, code, n FROM deleted;
  INSERT klog SELECT 'I', id, code, n FROM inserted;`,
      "UPDATE k SET id = id + 100, code = code + 'x' WHERE id = 1",
      'UPDATE x SET code = s.code FROM k AS x JOIN src s ON s.id = x.id',
      "WITH doomed AS (SELECT id FROM src WHERE code = 'yy') DELETE k FROM k JOIN doomed ON doomed.id = k.id",
      'SELECT op, id, code, n FROM klog ORDER BY op, id, code',
      'SELECT id, code, n FROM k ORDER BY id',
    ],
  },
  {
    name: 'RAISERROR with ROLLBACK in a trigger ends the transaction and the batch',
    batches: [
      'CREATE TABLE t2(id INT PRIMARY KEY, v INT); CREATE TABLE log2(msg VARCHAR(100))',
      `CREATE TRIGGER t2_ins ON t2 AFTER INSERT AS
  IF EXISTS (SELECT 1 FROM inserted WHERE v < 0)
  BEGIN
    RAISERROR('negative', 16, 1);
    ROLLBACK TRANSACTION;
    INSERT log2 SELECT 'after rollback rows=' + CAST(COUNT(*) AS VARCHAR) FROM inserted;
    RETURN
  END
  INSERT log2 SELECT 'ok tc=' + CAST(@@TRANCOUNT AS VARCHAR) + ' rows=' + CAST(COUNT(*) AS VARCHAR) FROM inserted`,
      'INSERT t2 VALUES (1, 1)',
      "INSERT t2 VALUES (2, -1) SELECT 'after' AS x",
      'SELECT id, v FROM t2; SELECT msg FROM log2 ORDER BY msg',
      "BEGIN TRAN INSERT t2 VALUES (3, 3) INSERT t2 VALUES (4, -4) SELECT 'after2'",
      'SELECT @@TRANCOUNT AS tc; SELECT id FROM t2 ORDER BY id',
      'DELETE log2',
      `BEGIN TRY
  INSERT t2 VALUES (5, -5)
  SELECT 'not here'
END TRY
BEGIN CATCH
  SELECT ERROR_NUMBER() AS en, @@TRANCOUNT AS tc, XACT_STATE() AS xs
END CATCH
SELECT 'after catch' AS x`,
      'SELECT id FROM t2 ORDER BY id; SELECT msg FROM log2 ORDER BY msg',
    ],
  },
  {
    name: 'THROW and runtime errors in triggers',
    batches: [
      'CREATE TABLE t4(id INT PRIMARY KEY); CREATE TABLE log4(msg VARCHAR(100)); CREATE TABLE t5(id INT PRIMARY KEY, v INT)',
      `CREATE TRIGGER t4_throw ON t4 AFTER INSERT AS
  INSERT log4 VALUES ('t4 before throw');
  THROW 50001, 'thrown', 1;
  INSERT log4 VALUES ('t4 after throw');`,
      "INSERT t4 VALUES (1) SELECT 'after t4' AS x",
      'SELECT @@TRANCOUNT AS tc, (SELECT COUNT(*) FROM t4) AS n4, (SELECT COUNT(*) FROM log4) AS nl',
      "BEGIN TRAN INSERT log4 VALUES ('in tran') INSERT t4 VALUES (2) SELECT 'after t4 tran' AS x",
      'SELECT @@TRANCOUNT AS tc, (SELECT COUNT(*) FROM log4) AS nl',
      `BEGIN TRAN
BEGIN TRY
  INSERT t4 VALUES (3)
END TRY
BEGIN CATCH
  SELECT ERROR_NUMBER() AS en, ERROR_MESSAGE() AS em, @@TRANCOUNT AS tc, XACT_STATE() AS xs
END CATCH
SELECT @@TRANCOUNT AS tc2
IF @@TRANCOUNT > 0 ROLLBACK`,
      `CREATE TRIGGER t5_r ON t5 AFTER INSERT AS
  RAISERROR('warn16', 16, 1)
  INSERT log4 VALUES ('after raiserror')`,
      "INSERT t5 VALUES (1, 1) SELECT 'next' AS x, @@ERROR AS e",
      `CREATE TRIGGER t5_div ON t5 AFTER UPDATE AS
  INSERT log4 VALUES ('before div');
  DECLARE @x INT = (SELECT 1 / MIN(v - v) FROM inserted);
  INSERT log4 VALUES ('after div');`,
      "UPDATE t5 SET v = 2 SELECT 'next2' AS x",
      `CREATE TRIGGER t5_pk ON t5 AFTER DELETE AS
  INSERT log4 VALUES ('before pk');
  INSERT t5 VALUES (1, 1);
  INSERT log4 VALUES ('after pk');`,
      'INSERT t5 VALUES (2, 2)',
      "DELETE t5 WHERE id = 2 SELECT 'next3' AS x",
      'SELECT id, v FROM t5 ORDER BY id; SELECT msg FROM log4 ORDER BY msg',
    ],
  },
  {
    name: 'INSTEAD OF triggers',
    batches: [
      'CREATE TABLE t6(id INT IDENTITY(10,1) PRIMARY KEY, name VARCHAR(20), qty INT DEFAULT 7, stamp INT); CREATE TABLE log6(msg VARCHAR(200))',
      `CREATE TRIGGER t6_io ON t6 INSTEAD OF INSERT AS
  INSERT log6 SELECT 'io rc=' + CAST(@@ROWCOUNT AS VARCHAR) + ' id=' + ISNULL(CAST(id AS VARCHAR),'null') + ' qty=' + ISNULL(CAST(qty AS VARCHAR),'null') + ' name=' + ISNULL(name,'null') FROM inserted
  INSERT t6(name, qty, stamp) SELECT UPPER(name), qty, 1 FROM inserted`,
      "INSERT t6(name) VALUES ('a'), ('b') SELECT @@ROWCOUNT AS rc",
      'SELECT id, name, qty, stamp FROM t6 ORDER BY id; SELECT msg FROM log6 ORDER BY msg',
      `CREATE TRIGGER t6_iou ON t6 INSTEAD OF UPDATE, DELETE AS
  INSERT log6 SELECT 'iou i=' + CAST((SELECT COUNT(*) FROM inserted) AS VARCHAR) + ' d=' + CAST((SELECT COUNT(*) FROM deleted) AS VARCHAR) + ' new=' + ISNULL(CAST((SELECT SUM(qty) FROM inserted) AS VARCHAR), 'none')`,
      'UPDATE t6 SET qty = 100 SELECT @@ROWCOUNT AS rc DELETE t6 SELECT @@ROWCOUNT AS rc',
      "SELECT id, name, qty, stamp FROM t6 ORDER BY id; SELECT msg FROM log6 WHERE msg LIKE 'iou%' ORDER BY msg",
      'CREATE TRIGGER t6_io2 ON t6 INSTEAD OF INSERT AS SELECT 1',
      'INSERT t6 OUTPUT inserted.id VALUES (DEFAULT, 1, 1)',
    ],
  },
  {
    name: 'nested triggers, recursion, DISABLE/ENABLE and DROP',
    batches: [
      'CREATE TABLE a7(id INT, v INT); CREATE TABLE b7(id INT, v INT); CREATE TABLE log7(msg VARCHAR(200))',
      `CREATE TRIGGER a7_t ON a7 AFTER INSERT, UPDATE AS
  INSERT log7 SELECT 'a7 nest=' + CAST(TRIGGER_NESTLEVEL() AS VARCHAR) + ' i=' + CAST((SELECT COUNT(*) FROM inserted) AS VARCHAR)
  INSERT b7 SELECT id, v FROM inserted
  UPDATE a7 SET v = v + 100 WHERE id IN (SELECT id FROM inserted)`,
      `CREATE TRIGGER b7_t ON b7 AFTER INSERT AS
  INSERT log7 SELECT 'b7 nest=' + CAST(TRIGGER_NESTLEVEL() AS VARCHAR) + ' i=' + CAST((SELECT COUNT(*) FROM inserted) AS VARCHAR)`,
      "CREATE TRIGGER b7_t2 ON b7 AFTER INSERT AS INSERT log7 SELECT 'b7 second'",
      'INSERT a7 VALUES (1, 1), (2, 2)',
      'SELECT msg FROM log7 ORDER BY msg; SELECT id, v FROM a7 ORDER BY id; SELECT id, v FROM b7 ORDER BY id',
      'DISABLE TRIGGER b7_t ON b7 INSERT b7 VALUES (3,3) SELECT msg FROM log7 ORDER BY msg',
      'ENABLE TRIGGER ALL ON b7',
      'ALTER TABLE b7 DISABLE TRIGGER ALL',
      'INSERT b7 VALUES (4, 4) SELECT COUNT(*) AS n FROM log7',
      'ALTER TABLE b7 ENABLE TRIGGER b7_t, b7_t2',
      'INSERT b7 VALUES (5, 5) SELECT COUNT(*) AS n FROM log7',
      'DISABLE TRIGGER nosuch ON b7',
      'DISABLE TRIGGER b7_t ON a7',
      'DISABLE TRIGGER ALL ON nosuchtable',
      'ALTER TABLE b7 DISABLE TRIGGER nosuch',
      'DROP TRIGGER nosuch',
      'DROP TRIGGER IF EXISTS nosuch',
      'DROP TRIGGER b7_t2, nosuch, a7_t',
      "SELECT name FROM sys.objects WHERE type = 'TR' ORDER BY name",
      'DROP TABLE b7',
      "SELECT name FROM sys.objects WHERE type = 'TR' ORDER BY name",
      'CREATE TABLE ra(id INT, n INT); CREATE TABLE rb(id INT, n INT)',
      'CREATE TRIGGER ra_t ON ra AFTER INSERT AS INSERT rb SELECT id, n + 1 FROM inserted',
      'CREATE TRIGGER rb_t ON rb AFTER INSERT AS INSERT ra SELECT id, n + 1 FROM inserted',
      "INSERT ra VALUES (1, 0) SELECT 'after' AS x",
      'SELECT (SELECT COUNT(*) FROM ra) AS a, (SELECT COUNT(*) FROM rb) AS b, @@TRANCOUNT AS tc',
    ],
  },
  {
    name: 'trigger definition errors and OUTPUT',
    batches: [
      'CREATE TABLE t8(id INT, v INT); CREATE TABLE t8s(id INT, v INT)',
      'CREATE TRIGGER t8_x ON nosuch AFTER INSERT AS SELECT 1',
      'SELECT 1\nCREATE TRIGGER t8_x ON t8 AFTER INSERT AS SELECT 1',
      'CREATE TRIGGER t8 ON t8 AFTER INSERT AS SELECT 1',
      "CREATE TRIGGER t8_x ON t8 AFTER INSERT AS PRINT 'x'",
      "CREATE TRIGGER t8_x ON t8 AFTER INSERT AS PRINT 'y'",
      "ALTER TRIGGER t8_x ON t8 AFTER UPDATE AS PRINT 'z';",
      "CREATE OR ALTER TRIGGER t8_x ON t8 FOR INSERT, UPDATE AS PRINT 'w'",
      "ALTER TRIGGER t8_nope ON t8 AFTER UPDATE AS PRINT 'z'",
      "CREATE OR ALTER TRIGGER dbo.t8_y ON dbo.t8 WITH EXECUTE AS CALLER AFTER DELETE NOT FOR REPLICATION AS BEGIN SET NOCOUNT ON; PRINT 'y' END",
      "ALTER TRIGGER t8_x ON t8s AFTER INSERT AS PRINT 'moved'",
      "CREATE TRIGGER s9.t8_s ON t8 AFTER INSERT AS PRINT 's'",
      'INSERT t8 OUTPUT inserted.id VALUES (1, 1)',
      'INSERT t8 OUTPUT inserted.id, inserted.v INTO t8s VALUES (2, 2)',
      'SELECT id, v FROM t8s',
      'CREATE TRIGGER t8_ae ON t8 AFTER INSERT, INSERT AS PRINT 1',
      'CREATE VIEW v8 AS SELECT * FROM t8',
      'CREATE TRIGGER v8_t ON v8 AFTER INSERT AS PRINT 1',
      'CREATE TRIGGER t8_z ON t8 AFTER INSERT AS',
      'TRUNCATE TABLE t8',
    ],
  },
  {
    name: 'transaction control inside triggers',
    batches: [
      'CREATE TABLE t9(id INT PRIMARY KEY, v INT); CREATE TABLE log9(msg VARCHAR(200))',
      `CREATE TRIGGER t9_c ON t9 AFTER INSERT AS
  INSERT log9 SELECT 'enter tc=' + CAST(@@TRANCOUNT AS VARCHAR);
  COMMIT;
  INSERT log9 VALUES ('after commit');`,
      "INSERT t9 VALUES (1,1) SELECT 'next' AS x",
      "BEGIN TRAN INSERT t9 VALUES (2,2) SELECT 'next2' AS x",
      'SELECT @@TRANCOUNT AS tc; SELECT id FROM t9 ORDER BY id; SELECT msg FROM log9 ORDER BY msg',
      'DROP TRIGGER t9_c',
      `CREATE TRIGGER t9_r ON t9 AFTER INSERT AS
  ROLLBACK;
  INSERT log9 SELECT 'after rollback rows=' + CAST(COUNT(*) AS VARCHAR) FROM inserted;
  SELECT 'trigger result' AS r;`,
      "INSERT t9 VALUES (5,5) SELECT 'next5' AS x",
      "SELECT id FROM t9 ORDER BY id; SELECT msg FROM log9 WHERE msg LIKE 'after rollback%'",
    ],
  },
  {
    name: 'MERGE fires AFTER triggers once per action, in INSERT, UPDATE, DELETE order',
    batches: [
      `CREATE TABLE m(id INT IDENTITY(10, 1) PRIMARY KEY, k INT UNIQUE, v INT, w INT DEFAULT 42, c AS v * 2)
CREATE TABLE mlog(seq INT IDENTITY, msg VARCHAR(400), cu VARBINARY(10))
CREATE TABLE ms(k INT, v INT)
CREATE TABLE mout(act NVARCHAR(10), iid INT, did INT)
INSERT m(k, v, w) VALUES (1, 1, 1), (2, 2, 2), (3, 3, 3)
INSERT ms VALUES (2, 20), (3, 30), (4, 40)`,
      `CREATE TRIGGER m_iud ON m AFTER INSERT, UPDATE, DELETE AS
  DECLARE @rc INT = @@ROWCOUNT;
  INSERT mlog(msg, cu) SELECT CONCAT('iud rc=', @rc, ' tc=', @@TRANCOUNT,
    ' i=', (SELECT STRING_AGG(CONCAT(id, ':', k, ':', v, ':', w, ':', c), ',') WITHIN GROUP (ORDER BY id) FROM inserted),
    ' d=', (SELECT STRING_AGG(CONCAT(id, ':', k, ':', v, ':', w, ':', c), ',') WITHIN GROUP (ORDER BY id) FROM deleted),
    ' uv=', CASE WHEN UPDATE(v) THEN 1 ELSE 0 END, ' uw=', CASE WHEN UPDATE(w) THEN 1 ELSE 0 END,
    ' nest=', TRIGGER_NESTLEVEL()), COLUMNS_UPDATED();
  SELECT 'trigger set' AS ts, (SELECT COUNT(*) FROM inserted) AS i, (SELECT COUNT(*) FROM deleted) AS d`,
      "CREATE TRIGGER m_i ON m AFTER INSERT AS INSERT mlog(msg) SELECT 'i-only rc=' + CAST(@@ROWCOUNT AS VARCHAR)",
      "CREATE TRIGGER m_d ON m AFTER DELETE AS INSERT mlog(msg) SELECT 'd-only rc=' + CAST(@@ROWCOUNT AS VARCHAR)",
      `MERGE m USING ms ON ms.k = m.k
WHEN NOT MATCHED BY SOURCE AND m.k = 1 THEN UPDATE SET w = 7
WHEN NOT MATCHED BY SOURCE THEN DELETE
WHEN MATCHED THEN UPDATE SET v = ms.v
WHEN NOT MATCHED THEN INSERT (k, v) VALUES (ms.k, ms.v)
OUTPUT $action, inserted.id, deleted.id INTO mout;
SELECT @@ROWCOUNT AS rc`,
      'SELECT seq, msg, cu FROM mlog ORDER BY seq; SELECT id, k, v, w, c FROM m ORDER BY id; SELECT act, iid, did FROM mout ORDER BY act, iid',
      'DELETE mlog',
      `MERGE m USING (SELECT 99 AS k) AS s ON s.k = m.k
WHEN MATCHED AND 1 = 0 THEN DELETE
WHEN MATCHED THEN UPDATE SET w = 5
WHEN NOT MATCHED BY SOURCE AND m.k > 100 THEN DELETE;
SELECT @@ROWCOUNT AS rc`,
      'MERGE m USING ms ON ms.k = m.k WHEN MATCHED THEN UPDATE SET k = m.k + 100;',
      'WITH src AS (SELECT k, v FROM ms WHERE k = 4) MERGE dbo.m AS x USING src ON src.k = x.k WHEN NOT MATCHED BY SOURCE AND x.k = 1 THEN DELETE; SELECT @@ROWCOUNT AS rc',
      'SELECT seq, msg, cu FROM mlog ORDER BY seq; SELECT id, k, v, w, c FROM m ORDER BY id',
      'MERGE m USING ms ON ms.k = m.k WHEN MATCHED THEN UPDATE SET v = ms.v + 1 OUTPUT $action, inserted.id;',
      'MERGE dbo.m AS x USING ms ON ms.k = x.k WHEN MATCHED THEN DELETE OUTPUT $action, deleted.id;',
      'DROP TRIGGER m_iud, m_i',
      'MERGE m USING ms ON ms.k = m.k WHEN MATCHED THEN UPDATE SET v = 0 OUTPUT $action, inserted.id;',
      'DISABLE TRIGGER m_d ON m',
      'DELETE mlog',
      'MERGE m USING ms ON ms.k = m.k WHEN NOT MATCHED BY SOURCE THEN DELETE OUTPUT $action, deleted.k; SELECT COUNT(*) AS n FROM mlog',
    ],
  },
  {
    name: 'MERGE trigger errors, ROLLBACK and COMMIT',
    batches: [
      'CREATE TABLE me(id INT PRIMARY KEY, v INT); CREATE TABLE melog(msg VARCHAR(300)); CREATE TABLE mes(id INT, v INT); INSERT me VALUES (1, 1), (2, 2); INSERT mes VALUES (2, 20), (3, -30)',
      `CREATE TRIGGER me_ins ON me AFTER INSERT AS
  IF EXISTS (SELECT 1 FROM inserted WHERE v < 0)
  BEGIN
    RAISERROR('negative', 16, 1);
    ROLLBACK TRANSACTION;
    INSERT melog SELECT 'after rollback rows=' + CAST(COUNT(*) AS VARCHAR) FROM inserted;
    RETURN
  END
  INSERT melog SELECT 'ok tc=' + CAST(@@TRANCOUNT AS VARCHAR)`,
      "CREATE TRIGGER me_upd ON me AFTER UPDATE AS INSERT melog SELECT 'upd rows=' + CAST(COUNT(*) AS VARCHAR) FROM inserted",
      "MERGE me USING mes ON mes.id = me.id WHEN MATCHED THEN UPDATE SET v = mes.v WHEN NOT MATCHED THEN INSERT VALUES (mes.id, mes.v); SELECT 'after' AS x",
      'SELECT @@TRANCOUNT AS tc; SELECT id, v FROM me ORDER BY id; SELECT msg FROM melog ORDER BY msg',
      "BEGIN TRAN; INSERT melog VALUES ('in tran'); MERGE me USING mes ON mes.id = me.id WHEN MATCHED THEN UPDATE SET v = mes.v WHEN NOT MATCHED THEN INSERT VALUES (mes.id, mes.v); SELECT 'after2' AS x",
      'SELECT @@TRANCOUNT AS tc; SELECT id, v FROM me ORDER BY id; SELECT msg FROM melog ORDER BY msg',
      'DROP TRIGGER me_ins',
      "CREATE TRIGGER me_throw ON me AFTER DELETE AS INSERT melog VALUES ('before throw'); THROW 50001, 'thrown', 1;",
      'INSERT me VALUES (5, 5)',
      "MERGE me USING mes ON mes.id = me.id WHEN NOT MATCHED BY SOURCE THEN DELETE; SELECT 'after3' AS x",
      'SELECT @@TRANCOUNT AS tc; SELECT id, v FROM me ORDER BY id; SELECT msg FROM melog ORDER BY msg',
      `BEGIN TRAN
BEGIN TRY
  MERGE me USING mes ON mes.id = me.id WHEN NOT MATCHED BY SOURCE THEN DELETE;
END TRY
BEGIN CATCH
  SELECT ERROR_NUMBER() AS en, ERROR_MESSAGE() AS em, @@TRANCOUNT AS tc, XACT_STATE() AS xs
END CATCH
SELECT @@TRANCOUNT AS tc2
IF @@TRANCOUNT > 0 ROLLBACK`,
      `BEGIN TRY
  MERGE me USING mes ON mes.id = me.id WHEN NOT MATCHED BY SOURCE THEN DELETE;
END TRY
BEGIN CATCH
  SELECT ERROR_NUMBER() AS en, @@TRANCOUNT AS tc, XACT_STATE() AS xs
END CATCH`,
      'DROP TRIGGER me_throw',
      "CREATE TRIGGER me_commit ON me AFTER UPDATE AS COMMIT; INSERT melog VALUES ('after commit')",
      "MERGE me USING mes ON mes.id = me.id WHEN MATCHED THEN UPDATE SET v = 99; SELECT 'after4' AS x",
      'SELECT @@TRANCOUNT AS tc; SELECT id, v FROM me ORDER BY id; SELECT msg FROM melog ORDER BY msg',
    ],
  },
  {
    name: 'MERGE with INSTEAD OF triggers',
    batches: [
      'CREATE TABLE mi(id INT IDENTITY(10, 1) PRIMARY KEY, k INT, v INT DEFAULT 7); CREATE TABLE milog(seq INT IDENTITY, msg VARCHAR(300)); CREATE TABLE mis(k INT, v INT); CREATE TABLE miout(act NVARCHAR(10), id INT); INSERT mi(k, v) VALUES (1, 1), (2, 2); INSERT mis VALUES (2, 20), (3, 30)',
      `CREATE TRIGGER mi_ioi ON mi INSTEAD OF INSERT AS
  INSERT milog(msg) SELECT CONCAT('ioi rc=', @@ROWCOUNT, ' i=', (SELECT STRING_AGG(CONCAT(id, ':', k, ':', v), ',') WITHIN GROUP (ORDER BY k) FROM inserted), ' d=', (SELECT COUNT(*) FROM deleted))`,
      'MERGE mi USING mis ON mis.k = mi.k WHEN MATCHED THEN UPDATE SET v = mis.v WHEN NOT MATCHED THEN INSERT (k) VALUES (mis.k); SELECT @@ROWCOUNT AS rc',
      'MERGE dbo.mi AS x USING mis ON mis.k = x.k WHEN MATCHED THEN DELETE WHEN NOT MATCHED THEN INSERT (k) VALUES (mis.k);',
      'MERGE mi USING mis ON mis.k = mi.k WHEN NOT MATCHED THEN INSERT (k) VALUES (mis.k); SELECT @@ROWCOUNT AS rc',
      'MERGE mi USING mis ON mis.k = mi.k WHEN MATCHED THEN UPDATE SET v = mis.v; SELECT @@ROWCOUNT AS rc',
      'SELECT seq, msg FROM milog ORDER BY seq; SELECT id, k, v FROM mi ORDER BY id',
      `CREATE TRIGGER mi_iou ON mi INSTEAD OF UPDATE, DELETE AS
  INSERT milog(msg) SELECT CONCAT('iou rc=', @@ROWCOUNT,
    ' i=', (SELECT STRING_AGG(CONCAT(id, ':', k, ':', v), ',') WITHIN GROUP (ORDER BY id) FROM inserted),
    ' d=', (SELECT STRING_AGG(CONCAT(id, ':', k, ':', v), ',') WITHIN GROUP (ORDER BY id) FROM deleted),
    ' uv=', CASE WHEN UPDATE(v) THEN 1 ELSE 0 END)`,
      "CREATE TRIGGER mi_after ON mi AFTER INSERT, UPDATE, DELETE AS INSERT milog(msg) SELECT 'after i=' + CAST((SELECT COUNT(*) FROM inserted) AS VARCHAR)",
      'DELETE milog',
      'MERGE mi USING mis ON mis.k = mi.k WHEN MATCHED THEN UPDATE SET v = mis.v + 1 WHEN NOT MATCHED THEN INSERT (k) VALUES (mis.k) WHEN NOT MATCHED BY SOURCE THEN DELETE; SELECT @@ROWCOUNT AS rc',
      'MERGE mi USING (VALUES (2, 9)) AS s(k, v) ON s.k = mi.k WHEN MATCHED THEN UPDATE SET v = s.v OUTPUT $action, inserted.v INTO miout; SELECT @@ROWCOUNT AS rc',
      'SELECT seq, msg FROM milog ORDER BY seq; SELECT id, k, v FROM mi ORDER BY id; SELECT act, id FROM miout',
      'DISABLE TRIGGER mi_ioi ON mi',
      'MERGE mi USING mis ON mis.k = mi.k WHEN MATCHED THEN UPDATE SET v = mis.v WHEN NOT MATCHED THEN INSERT (k) VALUES (mis.k); SELECT @@ROWCOUNT AS rc',
      'MERGE mi USING mis ON mis.k = mi.k WHEN NOT MATCHED THEN INSERT (k) VALUES (mis.k); SELECT @@ROWCOUNT AS rc',
      "SELECT seq, msg FROM milog WHERE seq > 3 ORDER BY seq; SELECT id, k, v FROM mi ORDER BY id",
    ],
  },
  {
    name: 'MERGE inside triggers: nesting and recursion',
    batches: [
      'CREATE TABLE na(id INT PRIMARY KEY, v INT); CREATE TABLE nb(id INT PRIMARY KEY, v INT); CREATE TABLE nlog(seq INT IDENTITY, msg VARCHAR(300)); CREATE TABLE ns(id INT, v INT); INSERT na VALUES (1, 1), (2, 2); INSERT ns VALUES (2, 20), (3, 30)',
      `CREATE TRIGGER na_t ON na AFTER INSERT, UPDATE AS
  INSERT nlog(msg) SELECT 'na nest=' + CAST(TRIGGER_NESTLEVEL() AS VARCHAR) + ' i=' + CAST(COUNT(*) AS VARCHAR) FROM inserted;
  MERGE nb USING inserted i ON i.id = nb.id WHEN MATCHED THEN UPDATE SET v = i.v WHEN NOT MATCHED THEN INSERT VALUES (i.id, i.v);
  MERGE na USING inserted i ON i.id = na.id WHEN MATCHED THEN UPDATE SET v = na.v + 1000;`,
      "CREATE TRIGGER nb_t ON nb AFTER INSERT, UPDATE AS INSERT nlog(msg) SELECT 'nb nest=' + CAST(TRIGGER_NESTLEVEL() AS VARCHAR) + ' i=' + CAST(COUNT(*) AS VARCHAR) + ' rc=' + CAST(@@ROWCOUNT AS VARCHAR) FROM inserted",
      'MERGE na USING ns ON ns.id = na.id WHEN MATCHED THEN UPDATE SET v = ns.v WHEN NOT MATCHED THEN INSERT VALUES (ns.id, ns.v); SELECT @@ROWCOUNT AS rc',
      'SELECT seq, msg FROM nlog ORDER BY seq; SELECT id, v FROM na ORDER BY id; SELECT id, v FROM nb ORDER BY id',
    ],
  },
  {
    name: 'MERGE report repro: a trivial AFTER INSERT trigger and an audit trigger',
    batches: [
      'CREATE TABLE items(id int PRIMARY KEY)',
      'CREATE TRIGGER foo_trigger ON items AFTER INSERT AS BEGIN SET NOCOUNT ON; END',
      'MERGE items AS target USING(VALUES(1)) AS source(id) ON target.id=source.id WHEN NOT MATCHED THEN INSERT(id) VALUES(source.id); SELECT @@ROWCOUNT AS rc',
      'SELECT id FROM items ORDER BY id',
      'CREATE TABLE stock(id INT PRIMARY KEY, qty INT); CREATE TABLE audit(seq INT IDENTITY, act CHAR(1), id INT, old_qty INT, new_qty INT)',
      `CREATE TRIGGER stock_audit ON stock AFTER INSERT, UPDATE AS
BEGIN
  SET NOCOUNT ON;
  INSERT audit(act, id, old_qty, new_qty)
  SELECT CASE WHEN d.id IS NULL THEN 'I' ELSE 'U' END, i.id, d.qty, i.qty
  FROM inserted i LEFT JOIN deleted d ON d.id = i.id ORDER BY i.id;
END`,
      'INSERT stock VALUES (1, 10)',
      'MERGE stock AS target USING (VALUES (1, 15), (2, 20)) AS source(id, qty) ON target.id = source.id WHEN MATCHED THEN UPDATE SET qty = source.qty WHEN NOT MATCHED THEN INSERT (id, qty) VALUES (source.id, source.qty); SELECT @@ROWCOUNT AS rc',
      'SELECT seq, act, id, old_qty, new_qty FROM audit ORDER BY seq; SELECT id, qty FROM stock ORDER BY id',
    ],
  },
]

function run(connection, sql) {
  return new Promise(resolve => {
    const result = { sql, sets: [], errors: [], info: [] }
    const done = []
    sinks.set(connection.config.options, done)
    const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, message: e.message })
    const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, message: e.message })
    const finish = error => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      sinks.delete(connection.config.options)
      result.done = done
      if (error && !result.errors.length) result.transport = { code: error.code ?? null, message: error.message }
      resolve(canonical(result))
    }
    const request = new Request(sql, error => finish(error))
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', metadata => result.sets.push({ columns: metadata.map(c => c.colName), rows: [] }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => Buffer.isBuffer(c.value) ? '0x' + c.value.toString('hex').toUpperCase() : c.value)))
    try { connection.execSqlBatch(request) } catch (error) { finish(error) }
  })
}

function close(connection) {
  if (connection.closed) return Promise.resolve()
  return new Promise(resolve => { connection.once('end', resolve); connection.close() })
}

async function observe(config) {
  const admin = await connect(config)
  const observations = []
  try {
    const [version] = (await run(admin, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128))")).sets[0].rows
    for (const [index, entry] of cases.entries()) {
      const database = `triggers_case_${index}`
      await run(admin, `CREATE DATABASE ${database}`)
      const connection = await connect({ ...config, options: { ...config.options, database } })
      try {
        const batches = []
        for (const sql of entry.batches) batches.push(await run(connection, sql))
        observations.push({ name: entry.name, batches })
        console.log(entry.name)
      } finally {
        await close(connection)
      }
    }
    return { version: version[0], cases: observations }
  } finally {
    await close(admin)
  }
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(new URL(import.meta.url).pathname)) {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  let capture
  await withReferenceContainer(async (config, container) => {
    if (container.image !== referenceImage) throw new Error('reference image must be pinned')
    capture = await observe(config)
  })
  const actual = { image: referenceImage, captureSha256: createHash('sha256').update(JSON.stringify(capture)).digest('hex'), ...capture }
  await writeFile(output, JSON.stringify(actual, null, 1) + '\n', { flag: 'wx' })
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${capture.cases.length} trigger cases from SQL Server ${capture.version}`)
}
