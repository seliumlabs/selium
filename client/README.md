# Selium Client

This is the client library for the Selium platform. Clients of the Selium server should
implement this library to send data to and/or receive data from the server.

## Getting Started

Once you've started a Selium server ([see the server's `README.md`](../server/README.md)
for details), use the client library to start sending and receiving messages.

Here's a minimal example:

```rust
use futures::SinkExt;
use selium::codecs::StringCodec;
use selium::prelude::*;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection = selium::client()
        .with_certificate_authority("/path/to/server.crt")? // your Selium server's cert
        .connect("127.0.0.1:7001") // your Selium server's address
        .await?;

    let mut publisher = connection
        .publisher("/some/topic") // choose a topic to group similar messages together
        .with_encoder(StringCodec) // allows you to exchange string messages between clients
        .open() // opens a new stream for sending data
        .await?;

    publisher.send("Hello, world!").await?;
    publisher.finish().await?;

    Ok(())
}
```

### Familiar Features

Selium Client uses Rust's `Futures` and `Tokio` libraries under the hood for doing
asynchronous I/O. If you are already familiar with these libs, then you already know how
to drive Selium!

Take our example above:

```rust
let mut publisher = connection
    .publisher("/some/topic")
    ...;
```

Here, `publisher` is actually just a `futures::Sink`. This makes it instantly compatible
with your existing streams/sinks:

```rust
let my_stream = ...; // whatever floats your boat!

my_stream.forward(publisher);
```

Now you're publishing messages!

There's lots more Selium can do, like sending things other than strings. Be sure to
[checkout the wiki](../../../wiki/Getting-Started) for more details.
