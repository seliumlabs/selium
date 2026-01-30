# Data Pipeline Example

A four-stage pipeline that transforms a stream of integers.

## Usage

### 1. Prepare your work directory (if not exists)

```bash
mkdir -p certs
mkdir -p modules
cargo run -p selium-runtime generate-certs
```

### 2. Build this example

```bash
cargo build -p selium-example-data-pipeline --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_example_data_pipeline.wasm modules/
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

### 5. Start the pipeline

Note that ordering is important, so follow the instructions in order.

In a new terminal, run:

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_data_pipeline.wasm generator \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```

In a second new terminal, run:

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    --attach \
    start selium_example_data_pipeline.wasm even \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```

> This terminal should start logging integers once the whole pipeline has been launched.

Back in the first terminal, run both:

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_data_pipeline.wasm double \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```

```bash
cd selium-modules/remote-client
cargo run -p selium-remote-cli -- \
    --cert-dir ../../certs \
    start selium_example_data_pipeline.wasm add_five \
    --capabilities ChannelLifecycle,ChannelReader,ChannelWriter,SingletonLookup
```
