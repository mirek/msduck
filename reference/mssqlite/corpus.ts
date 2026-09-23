import type { Case, ExpectedDifference } from './types.ts'

const difference =
  (
    path: string,
    mssqlite: unknown,
    sqlServer: unknown,
    reason: string
  ): ExpectedDifference => ({ path, mssqlite, sqlServer, reason })

const missing = { kind: 'missing' }

/** Reproductions from the compatibility TODO briefs present when this suite was proposed. */
export const corpus: readonly Case[] = [
  {
    name: 'scalar result metadata',
    sourceTodo: 'scalar-result-metadata',
    query: `
      SELECT N'ok' AS text_value,
        CAST('x' AS VARCHAR(5)) AS varchar_value,
        CAST(2 AS BIT) AS bit_value
    `
  },
  {
    name: 'system scalar result metadata',
    sourceTodo: 'scalar-result-metadata',
    query: `
      SELECT @@TRANCOUNT AS transaction_count,
        XACT_STATE() AS transaction_state
    `
  },
  {
    name: 'implicit type conversions',
    sourceTodo: 'implicit-type-conversions',
    query: `
      SELECT '1' + 2 AS arithmetic_value,
        CASE WHEN 1 = '1' THEN 1 ELSE 0 END AS comparison_value
    `
  },
  {
    name: 'string comparison padding',
    sourceTodo: 'string-comparison-padding',
    query: 'SELECT CASE WHEN \'a\' = \'a \' THEN 1 ELSE 0 END AS equal'
  },
  {
    name: 'table value constructor source',
    sourceTodo: 'table-value-constructors-in-from',
    query: `
      SELECT value
      FROM (VALUES (1), (NULL), (2)) AS source(value)
      ORDER BY value
    `
  },
  {
    name: 'ordered result token stream',
    sourceTodo: 'tds-order-token',
    todo: 'order-token-fidelity',
    query: `
      SELECT value
      FROM (VALUES (2), (1)) AS source(value)
      ORDER BY value
    `
  },
  {
    name: 'derived table apply',
    sourceTodo: 'apply-derived-tables',
    query: `
      SELECT p.id, q.n
      FROM (SELECT 1 AS id UNION ALL SELECT 2) AS p
      CROSS APPLY (SELECT p.id + 1 AS n) AS q
    `
  },
  {
    name: 'for xml path',
    sourceTodo: 'for-xml',
    query: 'SELECT 1 AS value FOR XML PATH(\'row\')',
    differences: [
      difference(
        '/execution/results/0/columns/0/type',
        'NVarChar', 'NText',
        'FOR XML text intentionally uses the brief-required nvarchar(max) rather than legacy ntext.'
      ),
      difference(
        '/execution/results/0/columns/0/length',
        65535, 2147483646,
        'nvarchar(max) and legacy ntext advertise different tedious data lengths.'
      )
    ]
  },
  {
    name: 'alter table alter column',
    sourceTodo: 'alter-table-alter-column',
    todo: 'rpc-completion-token-fidelity',
    setup: `
      DROP TABLE IF EXISTS differential_alter_probe;
      CREATE TABLE differential_alter_probe (value INT);
      INSERT INTO differential_alter_probe VALUES (7);
    `,
    query: `
      ALTER TABLE differential_alter_probe ALTER COLUMN value BIGINT NULL;
      SELECT value FROM differential_alter_probe;
    `,
    cleanup: 'DROP TABLE IF EXISTS differential_alter_probe',
    differences: [
      difference(
        '/execution/done/0/rowCount', 1, null,
        'mssqlite reports the rebuilt ALTER row count on its first completion token.'
      ),
      difference(
        '/execution/done/1/kind', 'doneProc', 'doneInProc',
        'SQL Server emits a separate statement completion before the final RPC completion.'
      ),
      difference(
        '/execution/done/1/rowCount', null, 1,
        'SQL Server reports the SELECT row count on its second completion token.'
      ),
      difference(
        '/execution/done/1/more', false, true,
        'SQL Server has another final RPC completion token after the SELECT.'
      ),
      difference(
        '/execution/done/2',
        missing,
        { kind: 'doneProc', rowCount: null, more: false },
        'SQL Server emits a final RPC completion after the two statement completions.'
      )
    ]
  },
  {
    name: 'identity seed and increment',
    sourceTodo: 'identity-semantics',
    setup: `
      DROP TABLE IF EXISTS differential_identity_probe;
      CREATE TABLE differential_identity_probe (
        id INT IDENTITY(10, 5) PRIMARY KEY,
        value INT
      );
      INSERT INTO differential_identity_probe (value) VALUES (1), (2);
    `,
    query: 'SELECT id FROM differential_identity_probe ORDER BY id',
    cleanup: 'DROP TABLE IF EXISTS differential_identity_probe'
  },
  {
    name: 'identity explicit value error',
    sourceTodo: 'identity-semantics',
    setup: `
      DROP TABLE IF EXISTS differential_identity_error;
      CREATE TABLE differential_identity_error (
        id INT IDENTITY(10, 5) PRIMARY KEY,
        value INT
      );
    `,
    query: 'INSERT INTO differential_identity_error (id, value) VALUES (100, 3)',
    cleanup: 'DROP TABLE IF EXISTS differential_identity_error'
  },
  {
    name: 'character width values',
    sourceTodo: 'character-width-enforcement',
    todo: 'character-cast-width-metadata',
    query: `
      SELECT CAST('abcdefghijklmnopqrstuvwxyz1234567890' AS VARCHAR) AS cast_value,
        ISNULL(CAST(NULL AS VARCHAR(3)), 'abcdef') AS isnull_value
    `,
    differences: [ difference(
      '/execution/results/0/columns/0/length',
      1, 30,
      'The value truncates at the CAST default of 30, but mssqlite still advertises varchar(1).'
    ) ]
  },
  {
    name: 'character width assignment error',
    sourceTodo: 'character-width-enforcement',
    setup: `
      DROP TABLE IF EXISTS differential_width_probe;
      CREATE TABLE differential_width_probe (value VARCHAR(3));
    `,
    query: 'INSERT INTO differential_width_probe VALUES (\'abcdef\')',
    cleanup: 'DROP TABLE IF EXISTS differential_width_probe'
  },
  {
    name: 'merge semicolon validation',
    sourceTodo: 'merge-validation',
    todo: 'runtime-error-stream-fidelity',
    setup: `
      DROP TABLE IF EXISTS differential_merge_probe;
      CREATE TABLE differential_merge_probe (id INT PRIMARY KEY, value INT);
    `,
    query: `
      MERGE differential_merge_probe AS t
      USING (VALUES (1, 2)) AS s(id, value)
      ON t.id = s.id
      WHEN NOT MATCHED THEN
        INSERT (id, value) VALUES (s.id, s.value)
    `,
    cleanup: 'DROP TABLE IF EXISTS differential_merge_probe',
    differences: [ difference(
      '/execution/error/lineNumber',
      1, 5,
      'mssqlite compatibility errors currently report the statement start line.'
    ) ]
  },
  {
    name: 'openjson strict missing path',
    sourceTodo: 'openjson-strict-paths',
    todo: 'runtime-error-stream-fidelity',
    query: `
      SELECT [key], value
      FROM OPENJSON(N'{"x":1}', 'strict $.missing')
    `,
    differences: [
      difference(
        '/execution/results/0',
        missing,
        {
          columns: [
            {
              name: 'key', type: 'NVarChar', length: 8000,
              precision: null, scale: null, nullable: false
            },
            {
              name: 'value', type: 'NVarChar', length: 65535,
              precision: null, scale: null, nullable: true
            }
          ],
          rows: []
        },
        'SQL Server sends OPENJSON COLMETADATA before the runtime strict-path error.'
      ),
      difference(
        '/execution/done/0/kind',
        'doneInProc', 'doneProc',
        'mssqlite terminates the failed RPC with an intermediate error completion first.'
      ),
      difference(
        '/execution/done/0/more',
        true, false,
        'mssqlite follows its error completion with a final RPC completion.'
      ),
      difference(
        '/execution/done/1',
        { kind: 'doneProc', rowCount: null, more: false },
        missing,
        'mssqlite emits a separate final RPC completion after this runtime error.'
      )
    ]
  },
  {
    name: 'select into type preservation',
    sourceTodo: 'select-into-type-preservation',
    todo: 'catalog-result-metadata',
    setup: 'DROP TABLE IF EXISTS differential_into_probe',
    query: `
      SELECT 1 AS id, CAST('x' AS VARCHAR(5)) AS value
      INTO differential_into_probe;
      SELECT c.name, TYPE_NAME(c.user_type_id) AS type_name, c.max_length
      FROM sys.columns AS c
      WHERE c.object_id = OBJECT_ID('differential_into_probe')
      ORDER BY c.column_id;
    `,
    cleanup: 'DROP TABLE IF EXISTS differential_into_probe',
    differences: [
      difference(
        '/execution/results/0/columns/0/length',
        65535, 256,
        'mssqlite sys.columns name is nvarchar(max), not SQL Server sysname width.'
      ),
      difference(
        '/execution/results/0/columns/1/length',
        65535, 256,
        'mssqlite TYPE_NAME output is nvarchar(max), not SQL Server sysname width.'
      ),
      difference(
        '/execution/results/0/columns/2/type',
        'IntN', 'SmallInt',
        'mssqlite exposes sys.columns.max_length through a nullable integer family.'
      ),
      difference(
        '/execution/results/0/columns/2/length',
        4, null,
        'IntN carries a width byte while fixed SMALLINT TYPE_INFO does not.'
      ),
      difference(
        '/execution/results/0/columns/2/nullable',
        true, false,
        'mssqlite catalog-view max_length metadata is currently nullable.'
      )
    ]
  },
  {
    name: 'unique null semantics',
    sourceTodo: 'unique-null-semantics',
    setup: `
      DROP TABLE IF EXISTS differential_unique_probe;
      CREATE TABLE differential_unique_probe (value INT UNIQUE);
      INSERT INTO differential_unique_probe VALUES (NULL);
    `,
    query: 'INSERT INTO differential_unique_probe VALUES (NULL)',
    cleanup: 'DROP TABLE IF EXISTS differential_unique_probe'
  }
]
