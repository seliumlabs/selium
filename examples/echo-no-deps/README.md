# Echo Example (no dependencies)

A simple echo client/server that demonstrates how to build a typed request/response flow without the Switchboard or Atlas Selium modules.

**Note that this approach is not recommended for general use**, but if you need to keep the footprint to a minimum, this is how you'd do it.

## Usage

### 1. Prepare your work directory (if not exists)

```bash
mkdir -p certs
mkdir -p modules
cargo run -p selium-runtime generate-certs
```

### 2. Build this example

```bash
cargo build -p selium-example-echo-no-deps --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_example_echo_no_deps.wasm modules/
```

### 3. Build runtime dependencies

To control a Selium Runtime from the outside, we need the `remote-client` module:

```bash
git clone https://github.com/seliumlabs/selium-modules
cd selium-modules/remote-client
cargo build -p selium-remote-client-server --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_remote_client_server.wasm ../../modules/
```

It is not possible to run this example without the remote client dependency, because we need to observe the server output before running the client (see below). For situations where this isn't necessary, you can run Selium Runtime without any dependencies.

### 4. Start the Selium Runtime

In a fresh terminal, run:

```bash
cargo run -p selium-runtime -- \
    --module 'path=selium_remote_client_server.wasm;capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,ProcessLifecycle,NetQuicBind,NetQuicAccept,NetQuicRead,NetQuicWrite;args=utf8:localhost,u16:7000'
```

The long `--module` definition tells the runtime to compile and run the remote client dependency with capabilities to create/read/write channels, and create/read/write QUIC sockets. Finally we provide two arguments to the remote client server, telling it to bind to localhost:7000.

### 5. Start the example echo server

In a fresh terminal, run:

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_echo_no_deps.wasm echo_server \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter
```

`--attach` tells the CLI to subscribe to the example's log channel so we can see what it's doing. `-a` identifies an argument to pass; in our case the HTTP address to bind. Finally we grant the example module the capability to create/read/write an HTTP socket.

**Note the `request_id` that is logged to the console:**
> [INFO] selium_example_echo_no_deps: echo server ready request_id=2

### 6. Run the example echo client

Last but not least, we can run the echo client, passing the server's shared request handle in place of `REQUEST_ID`:

```bash
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_echo_no_deps.wasm echo_client \
    --attach \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter \
    --arg resource:REQUEST_ID
```
