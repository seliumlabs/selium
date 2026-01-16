# Echo Example (no dependencies)

A simple echo client/server that demonstrates how to build a typed request/response flow without any Selium modules like Switchboard or Atlas.

## Usage

- Run the setup script:

```bash
./examples/setup_dev.sh
```

- Build & install the guest module:

```bash
cargo build --target wasm32-unknown-unknown --manifest-path examples/echo/Cargo.toml
cp examples/echo/target/wasm32-unknown-unknown/debug/selium_example_echo.wasm modules/
```

- Start a Selium server runtime in another terminal:

```bash
cargo run -p selium-runtime
```

- Start the echo server via the CLI and note the logged `request_id`:

```bash
cargo run -p selium-cli -- start selium_example_echo.wasm echo_server \
  --capability channel-lifecycle \
  --capability channel-reader \
  --capability channel-writer \
  --attach
```

- Start the client, passing the server's shared request handle in place of `REQUEST_ID`:

```bash
cargo run -p selium-cli -- start selium_example_echo.wasm echo_client \
  --capability channel-lifecycle \
  --capability channel-reader \
  --capability channel-writer \
  --arg resource:REQUEST_ID \
  --attach
```
