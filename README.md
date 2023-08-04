# Selium

[![Crates.io][crates-badge]][crates-url]
[![MPL2 licensed][mpl-badge]][mpl-url]
[![Build Status][build-badge]][build-url]
[![Audit Status][audit-badge]][audit-url]

[crates-badge]: https://img.shields.io/crates/v/selium.svg
[crates-url]: https://crates.io/crates/selium
[mpl-badge]: https://img.shields.io/badge/licence-MPL2-blue.svg
[mpl-url]: https://github.com/seliumlabs/selium/blob/main/LICENCE
[build-badge]: https://github.com/seliumlabs/selium/actions/workflows/test.yml/badge.svg
[build-url]: https://github.com/seliumlabs/selium/actions/workflows/test.yml
[audit-badge]: https://github.com/seliumlabs/selium/actions/workflows/audit.yml/badge.svg
[audit-url]: https://github.com/seliumlabs/selium/actions/workflows/audit.yml

Selium is an extremely developer friendly, composable messaging platform with zero build time
configuration.

## Getting Started

### Hello World

First, create a new Cargo project:

```bash
$ cargo new --bin hello-world
$ cd hello-world/
$ cargo add futures
$ cargo add selium
$ cargo add -F macros,rt-multi-thread tokio
```

Copy the following code into `hello-world/src/main.rs`:

```rust
use futures::{SinkExt, StreamExt};
use selium::{codecs::StringCodec, prelude::*};
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let connection = selium::client()
        .with_certificate_authority("server.crt")?
        .connect("127.0.0.1:7001")
        .await?;
    let connection_c = connection.clone();

    let mut subscriber = connection
        .subscriber("/hello/world")
        .with_decoder(StringCodec)
        .open()
        .await?;

    tokio::spawn(async move {
        let mut publisher = connection_c
            .publisher("/hello/world")
            .with_encoder(StringCodec)
            .open()
            .await
            .unwrap();

        publisher.send("Hello, world!").await.unwrap();
        publisher.finish().await.unwrap();
    });

    println!("{}", subscriber.next().await.unwrap()?);

    Ok(())
}
```

Next, open a new terminal window and start a new Selium server:

```bash
$ cargo install selium-server
$ cargo run --bin selium-server -- --bind-addr=127.0.0.1:7001 --self-signed
```

Copy the certificate from stdout and paste the contents into a new file called
`hello-world/server.crt`.

Finally, in our original terminal window, run the client:

```bash
$ cargo run
```

### Next Steps

Selium is a brokered messaging platform, meaning that it has a client and a server component. Check
out the [`client`](client/) and [`server`](server/) crates for more details.

We also have [the wiki](../../wiki) that includes all of this information and much more. Our
[Getting Started](../../wiki/Getting-Started) guide will step you through the process of setting up
a secure, working Selium platform in 5 minutes or less.

## Contributing to Selium

We'd love your help! If there's a feature you want, raise an issue first to avoid disappointment.
While we're happy to merge contributions that are in line with our roadmap, your feature may not
quite fit. Best to check first.
