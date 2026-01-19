# Echo Example

A simple echo client/server that demonstrates how to build a typed request/response flow using the Switchboard and Atlas modules.

## Usage

### 1. Prepare your work directory (if not exists)

```bash
mkdir -p certs
mkdir -p modules
cargo run -p selium-runtime generate-certs
```

### 2. Build this example

```bash
cargo build -p selium-example-echo --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_example_echo.wasm modules/
```

### 3. Build runtime dependencies

To control a Selium Runtime from the outside, we need the `remote-client` module:

```bash
git clone https://github.com/seliumlabs/selium-modules
cd selium-modules/remote-client
cargo build -p selium-remote-client-server --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_remote_client_server.wasm modules/
```

Then, to automatically manage channels and provide composable messaging types, we need the `switchboard` module:

```bash
cd ../switchboard
cargo build -p selium-switchboard-server --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_switchboard_server.wasm modules/
```

Finally, to discover the example server, we need the `atlas` module:

```bash
cd ../atlas
cargo build -p selium-atlas-server --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_atlas_server.wasm modules/
```

### 4. Start the Selium Runtime

In a fresh terminal, run:

```bash
cargo run -p selium-runtime -- \
    --module 'path=selium_remote_client_server.wasm;capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,ProcessLifecycle,NetQuicBind,NetQuicAccept,NetQuicRead,NetQuicWrite;args=utf8:localhost,u16:7000' \
    --module 'path=selium_switchboard_server.wasm;capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,SingletonRegister' \
    --module 'path=selium_atlas_server.wasm;capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,SingletonRegister'
```

The long `--module` definitions tell the runtime to compile and run the remote client, switchboard, and atlas dependencies with their respective required capabilities. The remote client definition also includes two arguments, indicating that it should bind to localhost:7000.

### 5. Start the example echo server

In a fresh terminal, run:

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    start selium_example_echo.wasm echo_server \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup \
    --cert-dir ../../certs
```

`--attach` tells the CLI to subscribe to the example's log channel so we can see what it's doing. `-a` identifies an argument to pass; in our case the HTTP address to bind. Finally we grant the example module the capability to create/read/write an HTTP socket.

### 6. Run the example echo client

Last but not least, we can run the echo client:

```bash
cargo run -p selium-remote-cli -- \
    start selium_example_echo.wasm echo_client \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup \
    --cert-dir ../../certs
```
