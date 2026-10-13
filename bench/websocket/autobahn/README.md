# Autobahn TestSuite for std/websocket

`fuzzingclient.json` is the configuration used for the numbers in #976 (301 cases; 12.* and 13.*,
permessage-deflate, excluded because no server here enables it). Start the servers on the host
(`HOST=0.0.0.0`; velt: `bench/websocket/velt/server.vlt`, velt-raw, Bun, Node `ws`, as in the
`servers` list), then:

```sh
docker run --rm -v "$PWD/bench/websocket/autobahn:/ab" --add-host=host.docker.internal:host-gateway \
  crossbario/autobahn-testsuite wstest -m fuzzingclient -s /ab/fuzzingclient.json
```

Reports land in `reports/` (`index.json`, one HTML page per case).

## The client codec

`client.vlt` runs every case of Autobahn's fuzzingserver with `connectWebSocket`, echoing what it
receives (`fuzzingserver.json`):

```sh
docker run -d --name ab-server -v "$PWD/bench/websocket/autobahn:/ab" -p 9001:9001 \
  crossbario/autobahn-testsuite wstest -m fuzzingserver -s /ab/fuzzingserver.json
velt build --release bench/websocket/autobahn/client.vlt -o /tmp/ab-client && /tmp/ab-client
docker rm -f ab-server   # reports in bench/websocket/autobahn/reports-client/
```
