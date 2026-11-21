# Sync: polling replaced by push

The `marmot sync` command used to poll the server every 30 seconds. On the free tier that used up the
daily API quota before noon, so syncs started failing for everyone. We replaced polling with a websocket
push channel: the client opens one connection and the server announces changes.

Open item: reconnect backoff is a fixed 5 seconds for now.
