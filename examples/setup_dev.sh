#!/bin/sh
#
# DO NOT USE IN PRODUCTION!
# Setup script for development environments.
#

set -e

# Setup base directory
mkdir -p modules

# Generate TLS certificates
if [ ! -d "certs" ]; then
  cargo run -p selium-runtime -- generate-certs;
fi

# Build & install the Selium guest dependencies
cargo build -p selium-module-remote-client --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/selium_module_remote_client.wasm modules/
