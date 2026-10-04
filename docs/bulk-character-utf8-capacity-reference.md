# UTF8 BulkLoad capacity reference

Task #940 captures 128 isolated NULL-plus-challenge loads with matching VARCHAR(64)/bounded TYPE_INFO and VARCHAR(MAX)/PLP sources. Four primary targets are native UTF8 VARCHAR/CHAR and Unicode NVARCHAR/NCHAR; 16 selected controls use CP1251/CP1252 VARCHAR targets.

The input matrix separates native byte capacity from Unicode UTF16-unit capacity. It includes empty values, exact and overflowing ASCII/two-byte/supplementary values, ASCII spaces, NUL, NBSP, incomplete prefixes, an isolated continuation and an invalid-range sequence. Values are original bytes, not repaired client strings. Each load uses a separate table/connection so an earlier failure cannot hide a challenge.

Capture and final-source independent reproduction are pending. The collector retains complete original exchanges before validating derived semantic and wire pins. Unexpected admission errors, original session counters and client display differences remain evidence; this finite matrix does not establish a general decoder, padding rule or runtime implementation.
