# FLOAT declarations in VALUES

VALUES output inference now folds known REAL/FLOAT source declarations through
the existing numeric common-type rules, alongside decimal declarations. This
preserves source rounding and numeric precedence for aggregate operand binding
across aliases and CTEs. Untyped NULL is skipped when a declaration is known;
typed NULL still contributes its declared type. Unknown/incompatible inputs
remain unknown, without inventing a native/catalog type or duplicating sources.

Pure parser/binder tests cover REAL/FLOAT precision, mixed integer/decimal/NULL
inputs, derived and CTE names, unknown controls and volatile source occurrences.
This prerequisite accompanies FLOAT SUM/AVG runtime task #644 in one reviewed
PR; each file is covered by its separate exclusive claim. Final integrated
Rust/client/audit evidence is recorded in docs/float-aggregate-runtime.md.
