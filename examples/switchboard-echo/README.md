# Echo Example

A simple echo client/server that demonstrates how to build a typed request/response flow using the Switchboard module.

## Usage

### 1. Run the setup script:

```bash
./examples/setup_dev.sh
```

#### 2. Build & install the guest module:

```bash
cargo build --target wasm32-unknown-unknown --manifest-path examples/switchboard-echo/Cargo.toml
cp examples/switchboard-echo/target/wasm32-unknown-unknown/debug/selium_example_switchboard_echo.wasm modules/
```

#### 3. Start a Selium server runtime in another terminal:

```bash
cargo run -p selium-runtime
```

Look out for a log line like this one:

> INFO forward_log_stream{channel_id=0xa0b268290}: guest_target=selium_module_switchboard guest_spans=switchboard.start guest_fields=**request_channel=1** switchboard: created request channel

#### 4. Start the echo server via the CLI, passing the `request_channel` value in place of `SWITCHBOARD_CHANNEL`:

```bash
cargo run -p selium-cli -- start selium_example_switchboard_echo.wasm echo_server \
  --capability channel-lifecycle \
  --capability channel-reader \
  --capability channel-writer \
  --arg resource:SWITCHBOARD_CHANNEL \
  --attach
```

Look out for a log line like this one:

> [INFO] selium_example_switchboard_echo: echo server ready **server_endpoint=1**

#### 5. Start the client, passing the `server_endpoint` value in place of `SERVER_ENDPOINT`:

```bash
cargo run -p selium-cli -- start selium_example_switchboard_echo.wasm echo_client \
  --capability channel-lifecycle \
  --capability channel-reader \
  --capability channel-writer \
  --arg resource:SWITCHBOARD_CHANNEL \
  --arg u32:SERVER_ENDPOINT \
  --attach
```
