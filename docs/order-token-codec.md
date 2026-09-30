# Deterministic ORDER codec

`msduck_tds::order` encodes and decodes one complete TDS ORDER token over explicit
ordinal inputs. It has no database, parser, session, transport or environment
dependencies. `encode` appends only after validating the byte length and reserving
the whole token; errors preserve the caller's existing output. `decode` requires
an exact token boundary and rejects wrong tags, truncated/odd payloads and trailing
bytes before allocating the ordinal result.

The USHORT length counts bytes, allowing at most 32,767 two-byte ordinals. The
codec preserves their sequence, duplicates and all word values, including zero
for captured nonprojected sort keys. Direction is absent from the token. An empty
list encodes an empty payload; this representable format does not decide whether
such a token should be emitted for any query.

Tests compare every ORDER event in both retained SQL Server runs against exact
encoded bytes and decoded ordinals, including preparation and empty executions.
Boundary tests cover maximum even length, overflow without partial output, every
truncation of a representative token, odd maximum length and trailing data. See
[ORDER reference](order-token.md) for the live emission evidence and its limits.

This module does not yet change server responses. Resolved logical ordering
metadata and root emission during preparation/execution remain required. They
must preserve captured exceptions; syntactic ORDER BY alone cannot establish a
correct emission decision. Run `cargo test -p msduck-tds --test order` for this
codec's focused loop.
