# Load Balancer Example

A demo load balancer that combines HTTP ingress with fanout messaging.

## Usage

### 1. Prepare your work directory (if not exists)

```bash
mkdir -p certs
mkdir -p modules
cargo run -p selium-runtime generate-certs
```

### 2. Build this example

```bash
cargo build -p selium-example-load-balancer --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_example_load_balancer.wasm modules/
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
    start selium_example_load_balancer.wasm load_balancer \
    --attach \
    -a utf8:localhost -a u16:8080 \
    --capabilities ChannelLifecycle,ChannelWriter,NetQuicAccept,NetQuicBind,SingletonLookup
```

`--attach` tells the CLI to subscribe to the example's log channel so we can see what it's doing. `-a` identifies an argument to pass; in our case the HTTP address to bind. Finally we grant the example module the capability to create an HTTP socket, accept connections, write to channels, and lookup global singletons.

### 6. Run the example connection handler

Now start up one or more connection handlers. Work will be shared evenly between them.

```bash
cd ../remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_load_balancer.wasm conn_handler \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```

### 7. Test the load balancer

Last but not least, we can now send requests to the HTTP server:

```bash
curl localhost:8080 -d 'The internet\'s on computers now?'
```

You should see `curl` return "OK".
