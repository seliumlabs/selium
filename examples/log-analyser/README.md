# Traffic Analyser Example

An example of how to run out-of-band analysis of network traffic patterns.

## Usage

### 1. Prepare your work directory (if not exists)

```bash
mkdir -p certs
mkdir -p modules
cargo run -p selium-runtime generate-certs
```

### 2. Build this example

```bash
cargo build -p selium-example-log-analyser --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_example_log_analyser.wasm modules/
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

Start the log analyser app after the log producer is running (the analyser performs a one-time Atlas lookup). It looks for error and warning log floods on `sel://example/app/logs` over a 5 second window.

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_log_analyser.wasm analyser \
    -a utf8:sel://example/app/logs -a utf8:sel://example/logs/alerts -a u32:5 \
    --log-uri sel://example/logs/analyser \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```

Then to simulate a flood of warnings, run the "test_warnings" entrypoint:

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_log_analyser.wasm test_warnings \
    --log-uri sel://example/app/logs \
    --capabilities ChannelReader,SingletonLookup
```

Or to simulate a flood of errors, run the "test_errors" entrypoint:

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_log_analyser.wasm test_errors \
    --log-uri sel://example/app/logs \
    --capabilities ChannelReader,SingletonLookup
```
