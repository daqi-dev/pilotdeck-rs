# user_dialog HTTP store connection-lifecycle matrix (#702)

Node v22.22.0 (macOS arm64) raw-socket captures for the four states in
`_tmp/20261026-issue-702/plan.md` crossed with `server.close()`, a slow sender,
a peer FIN and a peer RST. The probe is plain `node:http` with no options; the
real TS entry `startGatewayUserDialogStoreHttpServer` also calls
`createServer(...)` with no options
(`resources/PilotDeck @cc7c4e6`,
`src/gateway/user-dialog/HttpGatewayUserDialogStore.ts:232,243`), so this is the
transport baseline. The TS handler itself reads the request body before
dispatching, unlike the Rust early-rejection deviation (registered as
`A7.4-HTTPSTORE-AUTH-BEFORE-BODY-001`); the `/early` cell probes the Node
transport's response/drain ordering, not the TS handler order.

Regenerate:

```sh
node src/pilotdeck-gateway/tests/fixtures/user-dialog-http-store/lifecycle-matrix/probe.mjs \
  src/pilotdeck-gateway/tests/fixtures/user-dialog-http-store/lifecycle-matrix/node-lifecycle-matrix.json
```

`generatedAt`, `Date` headers, ephemeral ports and timing values are
runtime-dependent. The compared facts are the status line, the
`Connection`/`Keep-Alive` headers, the body bytes and the terminal event
(`eof` / `error:ECONNRESET` / `deadline`). Small responses store full `hex` and
`text`; 2 MiB bodies store `length` + `sha256` + head/tail.

| Cell | State x event | Recorded Node behaviour |
|---|---|---|
| `s1_close_in_handler_before_head` | S1 x close | Handler calls `close()` before `writeHead`; response still advertises `Connection: keep-alive` + `Keep-Alive: timeout=5`; a second request on the same socket is answered. |
| `s1_close_while_slow_body` | S1 x close | Request head + 16 KiB of a declared 1 MiB body arrived, then `server.close()`: Node's transport invokes the handler at head time and streams a 2 MiB 200 response even though the request body is incomplete; the response is still being delivered at the 2 s deadline and `close()` is pending. The real TS handler instead waits for the full body before dispatch, so this cell records `node:http` order, not the TS server order. |
| `s3_body_flushing_close_reuses_same_socket` | S3 x close | Head advertised keep-alive, 2 MiB body still flushing (slow handler), client is a slow reader: `close()` keeps the connection; body is delivered and a second request on the same socket gets a 404. `server.close()` callback is still pending 2 s later (it waits for the keep-alive deadline). |
| `s3_body_flushing_client_fin` | S3 x FIN | The client half-closes right after the first 16,570-byte head/body chunk: Node does **not** finish the 2 MiB body; the client reads 0 further bytes and EOF. This differs from the small-response half-close golden in `../header-lifecycle/node-half-close.json`; #702 does not change the Rust half-close path. |
| `s3_body_flushing_client_rst` | S3 x RST | RST after the head: the already-read head remains, the rest of the body cannot arrive. |
| `s4_early_401_slow_sender` | S4 x slow sender | `writeHead(401); end()` before reading the body; the peer dribbles 3 MiB: the complete 269-byte 401 arrives and the connection stays keep-alive (`deadline`) because Node drains the unread body. |
| `s4_early_401_close` | S4 x close | Early 401, body still unread, then `server.close()`: the complete 269-byte 401 arrives; the connection stays keep-alive and `close()` is still pending 2 s later (Node drains the body). |
| `s4_early_401_client_fin` | S4 x FIN | Early 401, then client FIN with the body unread: the complete 401 arrives, then Node appends its 47-byte `HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n` and EOFs. |
| `s4_early_401_client_rst` | S4 x RST | Client RST before reading: no response bytes are observable by construction. |

Notes that matter for the Rust tests:

- A *fast* handler that writes a 2 MiB body can hand the whole body to the
  kernel before the client reads the head. Node's `server.close()` then treats
  the connection as idle and destroys it mid-delivery
  (`_tmp/20261026-issue-702/node/node-fast-buffered.out.txt`): the client read
  only ~33 KiB of a 2 MiB body and the follow-up got nothing. The Rust P1 test
  therefore uses an ~18 MiB response and does not read the body until after
  `close()`, keeping the body genuinely in flight.
- Node's drain of a slow unread body is unbounded by design (`_dump`); the Rust
  bounded drain (`100 ms` quiet period, `1 s` hard cap) is a resource-bound
  choice recorded in `A7.4-HTTPSTORE-AUTH-BEFORE-BODY-001-R3`.
