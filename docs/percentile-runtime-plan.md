# Percentile runtime planning

`msduck_sql::percentile::runtime_plan` consumes syntax and an explicit declaration
snapshot. It preserves one original fraction expression, one ordering expression
and the window. It reads no bound values or runtime state and exposes no converted
fraction. Numeric source syntax uses the captured lexical diagnostic rules;
1007/168 remain compile errors even in an unselected CASE branch. This never
validates a bound value or folds the fraction. CONT declaration intent is FLOAT(53); DISC still needs the caller's
ordering-source declaration. A successful plan does not invent unknown metadata.

The retained runtime matrix supports declared parameters, literals, arithmetic,
CAST, ABS, CASE, simple scalar SELECT without a row source, and zero-argument RAND.
Planning preserves CASE branches and RAND syntax; it neither evaluates nor
hoists them. Proven row dependencies reject with 8726/state1/class16, including
table-reading scalar queries. Unverified expressions and undeclared parameters
remain unknown barriers. Captured constant NULL ordering rejects with 5309.
The adapter must add preparation completion8180 when applicable; this pure plan
constructs only the underlying diagnostic.

NULL, malformed text, overflow and out-of-range fractions remain execution
inputs. They must not fail preparation or disappear from the syntax. The root
must evaluate statement-wide effects once, suppress the captured conversion/range
errors only for genuinely empty input, distinguish all-NULL ordering rows, and
preserve metadata/ORDER/error/DONE boundaries. This API does not execute any of
those steps, and existing static lowering remains unchanged.

`validate_sequence_source` accepts only the caller-isolated fraction tokens and
retains the captured NEXT VALUE FOR prohibition11720/state1/class15 without
accessing a sequence. ServerDialect does not yet parse this source. The future
frontend/root integration must isolate its fraction token scope before invoking
the check; scanning a whole batch would reject unrelated legal sequence uses.
Quoted names/string contents do not become keywords. No SQL text is substituted.

This is the deterministic planning stage toward runtime percentile support.
The root evaluator, query integration, prepared-handle reuse, complete descriptor
and token comparison, and frontend sequence support remain required work. Tests
against the finite reference are evidence about planning, not a server pass.
