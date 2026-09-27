# Server connection admission

`msduck` limits concurrently admitted TCP connections before cloning a DuckDB
connection or starting a worker thread. The default is 128. Set
`--max-connections N` to a positive integer from 1 through 1024. The limit
counts accepted sockets from admission until their worker exits, including
PRELOGIN, TLS handshake, authentication, active queries and idle sessions.

At capacity, the server closes the newly accepted socket without reading a TDS
request or inventing a SQL Server overload response. The atomic permit is
released after the worker returns, errors or panics; a failed thread spawn also
releases it. Admitted connections use the existing PRELOGIN, TLS, authentication
and session handling. This bounds server worker threads and DuckDB connection
clones, but admitted idle clients can occupy every slot. It does not bound the
operating system's TCP backlog or establish parity with SQL Server overload
diagnostics. Handshake timeouts and load shedding policy are separate work.

`tests/server_admission.rs` launches a finite child server with a one-connection
limit. It proves an idle post-PRELOGIN client holds a slot, a saturated socket
closes promptly, and releasing the slot allows a new client to log in and run a
query. It also checks invalid CLI limits without starting a listener.
