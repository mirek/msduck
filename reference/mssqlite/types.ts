export type Scalar =
  | null
  | boolean
  | number
  | string
  | { readonly kind: 'bigint', readonly value: string }
  | { readonly kind: 'binary', readonly value: string }
  | { readonly kind: 'date', readonly value: string }

export type Column = {
  readonly name: string,
  readonly type: string,
  readonly length: number | null,
  readonly precision: number | null,
  readonly scale: number | null,
  readonly nullable: boolean
}

export type ResultSet = {
  readonly columns: readonly Column[],
  readonly rows: readonly (readonly Scalar[])[]
}

export type Done = {
  readonly kind: 'done' | 'doneInProc' | 'doneProc',
  readonly rowCount: number | null,
  readonly more: boolean
}

export type ErrorSnapshot = {
  readonly number: number | null,
  readonly state: number | null,
  readonly class: number | null,
  readonly lineNumber: number | null,
  readonly message: string
}

export type Execution = {
  readonly results: readonly ResultSet[],
  readonly done: readonly Done[],
  readonly rowCount: number,
  readonly error?: ErrorSnapshot
}

export type SessionState = {
  readonly transactionCount: number,
  readonly transactionState: number,
  readonly reusable: boolean
}

export type Snapshot = {
  readonly execution: Execution,
  readonly session: SessionState
}

export type CommunicationStream = {
  readonly diagnostics: readonly string[],
  readonly tokens: readonly string[]
}

export type Communication = {
  readonly mssqlite: CommunicationStream,
  readonly sqlServer: CommunicationStream
}

export type ExpectedDifference = {
  readonly path: string,
  readonly mssqlite: unknown,
  readonly sqlServer: unknown,
  readonly reason: string
}

export type Case = {
  readonly name: string,
  readonly sourceTodo: string,
  readonly todo?: string,
  readonly setup?: string,
  readonly query: string,
  readonly cleanup?: string,
  readonly differences?: readonly ExpectedDifference[]
}

export type Difference = {
  readonly path: string,
  readonly mssqlite: unknown,
  readonly sqlServer: unknown
}

export type Comparison = {
  readonly unexpected: readonly Difference[],
  readonly unusedExpectations: readonly ExpectedDifference[]
}

export type CaseResult = {
  readonly case: Case,
  readonly mssqlite: Snapshot,
  readonly sqlServer: Snapshot,
  readonly communication: Communication,
  readonly comparison: Comparison,
  readonly reproduction: string
}
