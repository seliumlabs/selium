# REST API Example

A simple REST API over HTTP.

## Usage

### 1. Prepare your work directory (if not exists)

```bash
mkdir -p certs
mkdir -p modules
cargo run -p selium-runtime generate-certs
```

### 2. Build this example

```bash
cargo build -p selium-example-rest-api --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_example_rest_api.wasm modules/
```

### 3. Build runtime dependencies

To control a Selium Runtime from the outside, we need the `remote-client` module:

```bash
git clone https://github.com/seliumlabs/selium-modules
cd selium-modules/remote-client
cargo build -p selium-remote-client-server --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_remote_client_server.wasm modules/
```

### 4. Start the Selium Runtime

In a fresh terminal, run:

```bash
cargo run -p selium-runtime -- \
    --module 'path=selium_remote_client_server.wasm;capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,ProcessLifecycle,NetQuicBind,NetQuicAccept,NetQuicRead,NetQuicWrite;args=utf8:localhost,u16:7000'
```

The long `--module` definition tells the runtime to compile and run the remote client dependency with capabilities to create/read/write channels, and create/read/write QUIC sockets. Finally we provide two arguments to the remote client server, telling it to bind to localhost:7000.

### 5. Start this example

In a fresh terminal, run:

```bash
cargo run -p selium-remote-cli -- \
    start selium_example_http_rpc.wasm http_server \
    --attach \
    -a utf8:localhost -a u16:8080 \
    --capabilities NetHttpBind,NetHttpAccept,NetHttpRead,NetHttpWrite
```

`--attach` tells the CLI to subscribe to the example's log channel so we can see what it's doing. `-a` identifies an argument to pass; in our case the HTTP address to bind. Finally we grant the example module the capability to create/read/write an HTTP socket.

### 6. Test the example

Last but not least, we can now query the HTTP server:

```bash
$ curl localhost:8080 -d '{"password":"Its an illusion, Michael!"}'
{"status":true}
```

You should see `curl` return some JSON with a truthy status, indicating that you cracked the code!
