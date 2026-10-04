# BulkLoad character metadata reference

Task #945 separates declared source family/width from actual outgoing TYPE_INFO and target capacity. The 160 explicit probes include bounded widths1/8 and MAX source framing, bounded target widths1/8 and MAX, CHAR/VARCHAR family controls, isolated NULL/empty controls and declared-width-versus-target-capacity challenges. CP1252, CP1251 and UTF8 profiles are observed without changing runtime collation gates. Thirteen Unicode controls distinguish declaration units from wire byte widths.

Acquisition and final-source independent reproduction are pending. Fixed wire CHAR/NCHAR challenge values have their exact native wire width; target padding/capacity remains an observation. Original SQL, bytes, descriptors, diagnostics, callback/completion and session counters are retained before validation. Existing fixtures/helpers remain unchanged; no general admission rule or runtime endpoint implementation is claimed.
