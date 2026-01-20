# WAF Example

A minimal Web Application Firewall (WAF) flow that demonstrates how to build, route, and transform data pipelines.

## Usage

### 1. Prepare your work directory (if not exists)

```bash
mkdir -p certs
mkdir -p modules
cargo run -p selium-runtime generate-certs
```

### 2. Build this example

```bash
cargo build -p selium-example-waf --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_example_waf.wasm modules/
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

### 5. Start the example modules

Start the following entrypoints in order (each in a fresh terminal):

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_waf.wasm edge_ingress_stub \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_waf.wasm validate_requests \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_waf.wasm result_router \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_waf.wasm alert_sink \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_waf.wasm audit_sink \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```

You should see log output showing edge requests, verdicts, and core alert/audit fan-out.
