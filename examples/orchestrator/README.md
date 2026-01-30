# Orchestration Example

An orchestrator that spawns and wires processes, streams configuration updates, and collects results.

## Usage

### 1. Prepare your work directory (if not exists)

```bash
mkdir -p certs
mkdir -p modules
cargo run -p selium-runtime generate-certs
```

### 2. Build this example

```bash
cargo build -p selium-example-orchestrator --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_example_orchestrator.wasm modules/
```

### 3. Build runtime dependencies

To control a Selium Runtime from the outside, we need the `remote-client` module:

```bash
git clone https://github.com/seliumlabs/selium-modules
cd selium-modules/remote-client
cargo build -p selium-remote-client-server --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_remote_client_server.wasm ../../modules/
```

### 4. Start the Selium Runtime

In a fresh terminal, run:

```bash
cargo run -p selium-runtime -- \
    --module 'path=selium_remote_client_server.wasm;capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,ProcessLifecycle,NetQuicBind,NetQuicAccept,NetQuicRead,NetQuicWrite;args=utf8:localhost,u16:7000'
```

### 5. Start the orchestrator

In a fresh terminal, run:

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_orchestrator.wasm orchestrator \
    --attach \
    -a utf8:selium_example_orchestrator.wasm \
    -a utf8:worker-1 \
    -a u32:6 \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,ProcessLifecycle
```

The first argument (`selium_example_orchestrator.wasm`) tells the orchestrator which module to spawn for
its child processes. The next two arguments set the worker label and task count.

### 6. Observe the logs

The orchestrator will spawn a configuration publisher and a worker, push work items, and print the
results as configuration updates roll through.
