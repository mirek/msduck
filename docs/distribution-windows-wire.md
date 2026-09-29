# Distribution-window ERROR tokens

The pinned SQL Server 2025 capture in `reference/distribution-reference.json`
contains two independent runs for `PERCENT_RANK` and `CUME_DIST`. Their invalid
argument, missing OVER, missing ORDER BY and explicit-frame requests emit one
ERROR token followed by DONE_ERROR, with no result descriptor. Both functions
use class 15. The states are 1 for 4114 (argument), 3 for 10753 (missing OVER),
1 for 4112 (missing ORDER BY), and 3 for 10752 (frame).

The legacy TDS `error` adapter has only the number and message. It now selects
those class/state attributes only when both match an exact captured message for
one of the two functions; unrelated diagnostics retain their previous policy.
Wire tests decode the encoded ERROR token and compare its number, class, state
and UTF-16 message against both retained runs. Typed `SqlError` encoding still
uses the attributes supplied by its caller.

The root SQL validator change is separately claimed in `distribution-window-diagnostics-v1`.
Its public client test still expects the previous numbers and is reserved by
`ntile-null-runtime-v1`. This transport change alone does not close that
integration gap or establish compatibility for other ranking functions.
