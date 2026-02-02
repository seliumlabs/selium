# HTTPS Load Balancer Example

A demo load balancer that combines HTTPS ingress with fanout messaging.

This example ships with a self-signed certificate for `localhost` in the crate
root: `server-cert.pem` and `server-key.pem`.

## Usage

### 1. Prepare your work directory (if not exists)

```bash
mkdir -p certs
mkdir -p modules
cargo run -p selium-runtime generate-certs
```

### 2. Build this example

```bash
cargo build -p selium-example-load-balancer-https --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_example_load_balancer_https.wasm modules/
```

### 3. Build runtime dependencies

To control a Selium Runtime from the outside, we need the `remote-client` module:

```bash
git clone https://github.com/seliumlabs/selium-modules
cd selium-modules/remote-client
cargo build -p selium-remote-client-server --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_remote_client_server.wasm ../../modules/
```

Then, to manage channel wiring, we need the `switchboard` module:

```bash
cd ../switchboard
cargo build -p selium-switchboard-server --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_switchboard_server.wasm ../../modules/
```

Finally, to register and discover endpoints, we need the `atlas` module:

```bash
cd ../atlas
cargo build -p selium-atlas-server --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_atlas_server.wasm ../../modules/
```

### 4. Start the Selium Runtime

In a fresh terminal, run:

```bash
cargo run -p selium-runtime -- \
    --module 'path=selium_remote_client_server.wasm;capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,ProcessLifecycle,NetQuicBind,NetQuicAccept,NetQuicRead,NetQuicWrite;args=utf8:localhost,u16:7000' \
    --module 'path=selium_switchboard_server.wasm;capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,SingletonRegistry' \
    --module 'path=selium_atlas_server.wasm;capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,SingletonRegistry'
```

### 5. Start the load balancer

In a fresh terminal, run:

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_load_balancer_https.wasm load_balancer \
    --attach \
    -a utf8:localhost -a u16:8443 \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,NetHttpAccept,NetHttpBind,NetTlsServerConfig,SingletonLookup
```

`--attach` tells the CLI to subscribe to the example's log channel so we can see what it's doing. `-a` identifies an argument to pass; in our case the HTTPS address to bind. Finally we grant the example module the capability to create an HTTPS socket, register TLS material, accept connections, read/write channels, and lookup global singletons.

### 6. Run the example connection handler

Now start up one or more connection handlers. Work will be shared evenly between them.

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_load_balancer_https.wasm conn_handler \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,NetHttpRead,NetHttpWrite,SingletonLookup
```

### 7. Test the load balancer

Last but not least, we can now send requests to the HTTPS server:

```bash
curl --cacert examples/load-balancer-https/server-cert.pem \
    https://localhost:8443 -d "The internet's on computers now?"
```

You should see `curl` return "OK". If you prefer, you can use `curl -k` to
skip certificate verification.
