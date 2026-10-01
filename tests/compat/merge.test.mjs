// MERGE through tedious (issue #722): the generic upsert with and without
// table hints, every WHEN family, OUTPUT ($action, inserted, deleted, source
// columns and OUTPUT INTO), parameters, TOP and error numbers. Descriptors and
// values follow reference/merge-execution.json and reference/gaps-merge.json.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { start, query } from '../support/client.mjs'

const upsert = hint => `MERGE items ${hint} AS target USING (VALUES (1)) AS source(id) ON target.id = source.id WHEN NOT MATCHED THEN INSERT (id) VALUES (source.id);`

async function errorNumber (promise) {
  try {
    await promise
  } catch (error) {
    return error.number
  }
  assert.fail('expected an error')
}

test('generic upsert with and without table hints', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE TABLE items(id INT NOT NULL PRIMARY KEY)')
  for (const hint of ['', 'WITH(SERIALIZABLE)', 'WITH (HOLDLOCK)', 'WITH (UPDLOCK, ROWLOCK)', 'WITH (SERIALIZABLE, UPDLOCK)']) {
    await query(connection, 'DELETE FROM items')
    assert.equal((await query(connection, upsert(hint))).rowCount, 1, hint)
    assert.equal((await query(connection, upsert(hint))).rowCount, 0, hint)
    assert.deepEqual((await query(connection, 'SELECT id FROM items')).rows, [[1]])
  }
  // NOLOCK is not allowed on a MERGE target.
  assert.equal(await errorNumber(query(connection, upsert('WITH (NOLOCK)'))), 1065)
})

test('direct OUTPUT returns $action and both images', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE TABLE dbo.merge_target(id INT NOT NULL PRIMARY KEY,n INT NOT NULL); INSERT dbo.merge_target(id,n) VALUES (1,10),(2,20),(3,30)')
  let result = await query(connection, 'MERGE dbo.merge_target AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n OUTPUT $action AS action, inserted.id AS inserted_id, deleted.n AS prior_n, inserted.n AS current_n;')
  assert.deepEqual(result.rows, [['UPDATE', 1, 10, 11]])
  assert.equal(result.rowCount, 1)
  const [columns] = result.columns
  assert.deepEqual(columns.map(c => [c.colName, c.type.name]), [['action', 'NVarChar'], ['inserted_id', 'Int'], ['prior_n', 'Int'], ['current_n', 'Int']])
  assert.equal(columns[0].dataLength, 20)
  assert.equal(columns[0].flags & 1, 0)
  assert.deepEqual((await query(connection, 'SELECT @@ROWCOUNT')).rows, [[1]])

  result = await query(connection, 'MERGE dbo.merge_target AS t USING (VALUES (4,40)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n) OUTPUT $action, inserted.*, s.n AS source_n;')
  assert.deepEqual(result.rows, [['INSERT', 4, 40, 40]])
  assert.deepEqual(result.columns[0].map(c => c.colName), ['$action', 'id', 'n', 'source_n'])

  result = await query(connection, 'MERGE dbo.merge_target AS t USING (VALUES (1),(2),(4)) AS s(id) ON t.id=s.id WHEN NOT MATCHED BY SOURCE AND t.id=3 THEN DELETE OUTPUT $action AS action, deleted.id AS deleted_id, deleted.n AS prior_n;')
  assert.deepEqual(result.rows, [['DELETE', 3, 30]])

  // Mixed actions make the images nullable.
  result = await query(connection, `MERGE dbo.merge_target AS t
    USING (VALUES (1,12),(2,22),(5,50)) AS s(id,n) ON t.id=s.id
    WHEN MATCHED AND s.id=1 THEN UPDATE SET n=s.n
    WHEN MATCHED THEN DELETE
    WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n)
    OUTPUT $action AS action, inserted.id AS inserted_id, deleted.id AS deleted_id;`)
  assert.equal(result.rowCount, 3)
  assert.deepEqual(result.rows.sort((a, b) => `${a[0]}`.localeCompare(`${b[0]}`)), [['DELETE', null, 2], ['INSERT', 5, null], ['UPDATE', 1, 1]])
  assert.deepEqual(result.columns[0].map(c => [c.type.name, c.flags & 1]), [['NVarChar', 0], ['IntN', 1], ['IntN', 1]])
  assert.deepEqual((await query(connection, 'SELECT id, n FROM dbo.merge_target ORDER BY id')).rows, [[1, 12], [4, 40], [5, 50]])
})

test('OUTPUT INTO stores the action rows with the target write', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE TABLE dbo.merge_mix(id INT NOT NULL PRIMARY KEY,n INT NOT NULL); INSERT dbo.merge_mix(id,n) VALUES (1,10),(2,20),(3,30); CREATE TABLE dbo.merge_output([action] NVARCHAR(10) NOT NULL,inserted_id INT NULL,deleted_id INT NULL)')
  const result = await query(connection, `MERGE dbo.merge_mix AS t
    USING (VALUES (1,11),(2,22),(4,44)) AS s(id,n) ON t.id=s.id
    WHEN MATCHED AND s.id=1 THEN UPDATE SET n=s.n
    WHEN MATCHED AND s.id=2 THEN DELETE
    WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n)
    WHEN NOT MATCHED BY SOURCE AND t.id=3 THEN DELETE
    OUTPUT $action, inserted.id, deleted.id INTO dbo.merge_output([action],inserted_id,deleted_id);`)
  assert.equal(result.rowCount, 4)
  assert.deepEqual(result.rows, [])
  assert.deepEqual((await query(connection, 'SELECT @@ROWCOUNT')).rows, [[4]])
  assert.deepEqual((await query(connection, 'SELECT [action],inserted_id,deleted_id FROM dbo.merge_output ORDER BY [action],COALESCE(inserted_id,deleted_id)')).rows,
    [['DELETE', null, 2], ['DELETE', null, 3], ['INSERT', 4, null], ['UPDATE', 1, 1]])
  assert.deepEqual((await query(connection, 'SELECT id,n FROM dbo.merge_mix ORDER BY id')).rows, [[1, 11], [4, 44]])
})

test('parameters bind and identity values come back through OUTPUT', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE TABLE dbo.blogs(Id INT IDENTITY(1,1) PRIMARY KEY, Name NVARCHAR(50) NOT NULL, Rank INT NULL)')
  // The batched-insert shape EF Core generates.
  let result = await query(connection,
    'MERGE [dbo].[blogs] USING (VALUES (@p0, 0), (@p1, 1)) AS i ([Name], _Position) ON 1=0 WHEN NOT MATCHED THEN INSERT ([Name]) VALUES (i.[Name]) OUTPUT INSERTED.[Id], i._Position;',
    [['p0', TYPES.NVarChar, 'first'], ['p1', TYPES.NVarChar, 'second']])
  assert.deepEqual(result.rows.sort((a, b) => a[1] - b[1]), [[1, 0], [2, 1]])
  // A keyed upsert with every value a parameter.
  const save = (id, name, rank) => query(connection,
    'MERGE dbo.blogs WITH (HOLDLOCK) AS t USING (SELECT @id AS Id, @name AS Name, @rank AS Rank) AS s ON t.Id = s.Id WHEN MATCHED THEN UPDATE SET Name = s.Name, Rank = s.Rank WHEN NOT MATCHED THEN INSERT (Name, Rank) VALUES (s.Name, s.Rank) OUTPUT $action, inserted.Id;',
    [['id', TYPES.Int, id], ['name', TYPES.NVarChar, name], ['rank', TYPES.Int, rank]])
  assert.deepEqual((await save(2, 'renamed', 7)).rows, [['UPDATE', 2]])
  assert.deepEqual((await save(99, 'third', null)).rows, [['INSERT', 3]])
  assert.deepEqual((await query(connection, 'SELECT Id, Name, Rank FROM dbo.blogs ORDER BY Id')).rows,
    [[1, 'first', null], [2, 'renamed', 7], [3, 'third', null]])
})

test('errors keep their numbers and leave the target unchanged', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE TABLE dbo.merge_duplicate(id INT NOT NULL PRIMARY KEY,n INT NOT NULL CHECK (n > 0)); INSERT dbo.merge_duplicate(id,n) VALUES (1,10)')
  assert.equal(await errorNumber(query(connection, 'MERGE dbo.merge_duplicate AS t USING (VALUES (1,11),(1,12)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n;')), 8672)
  assert.equal(await errorNumber(query(connection, 'MERGE dbo.merge_duplicate AS t USING (VALUES (1,11),(2,-1)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);')), 547)
  assert.equal(await errorNumber(query(connection, 'MERGE TOP (-1) dbo.merge_duplicate AS t USING (VALUES (2,20)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n);')), 127)
  assert.equal(await errorNumber(query(connection, 'MERGE dbo.merge_duplicate AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN MATCHED AND s.n>0 THEN DELETE;')), 5324)
  assert.equal(await errorNumber(query(connection, 'MERGE dbo.merge_duplicate AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN DELETE')), 10713)
  assert.deepEqual((await query(connection, 'SELECT id,n FROM dbo.merge_duplicate')).rows, [[1, 10]])
  // TOP (1) updates a doubly matched row once.
  assert.equal((await query(connection, 'MERGE TOP (1) dbo.merge_duplicate AS t USING (VALUES (1,11),(1,12)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n;')).rowCount, 1)
})

test('a merge inside a transaction rolls back with it', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE TABLE dbo.merge_rollback(id INT NOT NULL PRIMARY KEY,n INT NOT NULL); INSERT dbo.merge_rollback(id,n) VALUES (1,10)')
  const result = await query(connection, `BEGIN TRANSACTION;
    MERGE dbo.merge_rollback AS t USING (VALUES (1,11),(2,20)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);
    SELECT id,n FROM dbo.merge_rollback ORDER BY id;
    ROLLBACK TRANSACTION;
    SELECT id,n FROM dbo.merge_rollback ORDER BY id`)
  assert.deepEqual(result.rows, [[1, 11], [2, 20], [1, 10]])
})

test('typed columns, defaults, computed columns and nested batches', async t => {
  const connection = await start(t)
  await query(connection, `CREATE TABLE [dbo].[orders]([Id] INT NOT NULL PRIMARY KEY, [Customer] NVARCHAR(40) NOT NULL, [Amount] DECIMAL(10,2) NOT NULL,
    [Paid] BIT NOT NULL DEFAULT 0, [Placed] DATETIME2(3) NULL, [Ref] UNIQUEIDENTIFIER NULL, [Status] VARCHAR(10) NOT NULL DEFAULT 'new', [Total] AS [Amount] * 2)`)
  let result = await query(connection, `MERGE INTO [dbo].[orders] WITH (HOLDLOCK) AS [o]
    USING (SELECT @id AS Id, @customer AS Customer, @amount AS Amount, @placed AS Placed, @ref AS Ref) AS [s] ON [o].[Id] = [s].[Id]
    WHEN MATCHED THEN UPDATE SET [Customer] = [s].[Customer], [Amount] = [s].[Amount], [Paid] = 1, [Status] = DEFAULT
    WHEN NOT MATCHED BY TARGET THEN INSERT ([Id], [Customer], [Amount], [Placed], [Ref]) VALUES ([s].[Id], [s].[Customer], [s].[Amount], [s].[Placed], [s].[Ref])
    OUTPUT $action, inserted.[Customer], inserted.[Amount], inserted.[Paid], inserted.[Status], inserted.[Total], deleted.[Amount] AS [Before];`,
  [['id', TYPES.Int, 1], ['customer', TYPES.NVarChar, 'Zoë'], ['amount', TYPES.Decimal, 12.345, { precision: 10, scale: 3 }],
    ['placed', TYPES.DateTime2, new Date('2026-01-02T03:04:05.678Z'), { scale: 3 }], ['ref', TYPES.UniqueIdentifier, '6F9619FF-8B86-D011-B42D-00C04FC964FF']])
  assert.deepEqual(result.rows, [['INSERT', 'Zoë', 12.35, false, 'new', 24.7, null]])
  result = await query(connection, `UPDATE dbo.orders SET Status = 'shipped';
    MERGE dbo.orders AS o USING (VALUES (1, N'Ann', 5)) AS s(Id, Customer, Amount) ON o.Id = s.Id
    WHEN MATCHED THEN UPDATE SET Customer = s.Customer, Amount = s.Amount, Paid = 1, Status = DEFAULT
    OUTPUT $action, inserted.Customer, inserted.Amount, inserted.Paid, inserted.Status, inserted.Total, deleted.Amount AS Before;`)
  assert.deepEqual(result.rows, [['UPDATE', 'Ann', 5, true, 'new', 10, 12.35]])
  const placed = (await query(connection, 'SELECT Placed, Ref FROM dbo.orders')).rows[0]
  assert.equal(placed[0].toISOString(), '2026-01-02T03:04:05.678Z')
  assert.equal(placed[1], '6F9619FF-8B86-D011-B42D-00C04FC964FF')

  // MERGE inside control flow and TRY...CATCH, with NOCOUNT.
  result = await query(connection, `SET NOCOUNT ON;
    DECLARE @n INT = 0;
    IF 1 = 1
    BEGIN
      MERGE dbo.orders AS o USING (VALUES (2, N'Bo', 1)) AS s(Id, Customer, Amount) ON o.Id = s.Id
        WHEN NOT MATCHED THEN INSERT (Id, Customer, Amount) VALUES (s.Id, s.Customer, s.Amount);
      SET @n = @@ROWCOUNT;
    END
    BEGIN TRY
      MERGE dbo.orders AS o USING (VALUES (3, NULL, 1)) AS s(Id, Customer, Amount) ON o.Id = s.Id
        WHEN NOT MATCHED THEN INSERT (Id, Customer, Amount) VALUES (s.Id, s.Customer, s.Amount);
    END TRY
    BEGIN CATCH
      SELECT @n AS inserted_rows, ERROR_NUMBER() AS error_number;
    END CATCH`)
  assert.deepEqual(result.rows, [[1, 515]])
  assert.deepEqual((await query(connection, 'SELECT Id FROM dbo.orders ORDER BY Id')).rows, [[1], [2]])
})
