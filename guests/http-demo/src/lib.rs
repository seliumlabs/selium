//! HTTP application demo guest.
//!
//! A real application guest behind the HTTP connector: it binds a served
//! route through the guest SDK's `HttpServe` surface (no `Network` grant —
//! the connector terminates TLS and forwards typed requests), accepts typed
//! sessions, and answers each request with a deterministic echo response.
//!
//! The connector resolves the route from the request's Host; a single-label
//! Host (for example `demo`) projects onto the root namespace, so this guest
//! serves `sel:///demo`.

use selium_guest::net::http::{HttpServe, HttpServeError, HttpServeStream};
use selium_guest::{Context, entrypoint, info, spawn, warn};
use selium_proto_http::HttpResponse;

/// The served unary route. A single-label request Host (`demo`) projects onto
/// the root namespace with that label as its base path, so a request for `/`
/// reaches `sel:///demo`.
const SERVE_PATH: &str = "demo";
/// The served streaming route: `GET /events` with Host `demo` reaches it.
const STREAM_PATH: &str = "demo/events";

#[entrypoint]
async fn http_demo(mut ctx: Context) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("http-demo: booting");

    let mut server = match HttpServe::bind(&mut ctx, SERVE_PATH).await {
        Ok(server) => server,
        Err(error) => {
            selium_guest::error!("http-demo: bind failed: {error:?}");
            return Err(anyhow::anyhow!("http-demo: bind failed: {error}"));
        }
    };
    info!("http-demo: serving {}", server.uri());

    let mut streaming = match HttpServeStream::bind(&mut ctx, STREAM_PATH).await {
        Ok(server) => server,
        Err(error) => {
            selium_guest::error!("http-demo: stream bind failed: {error:?}");
            return Err(anyhow::anyhow!("http-demo: stream bind failed: {error}"));
        }
    };
    info!("http-demo: serving stream {}", streaming.uri());
    spawn(async move {
        loop {
            let mut connection = match streaming.accept().await {
                Ok(connection) => connection,
                Err(HttpServeError::ConnectionClosed) => continue,
                Err(error) => {
                    warn!("http-demo: stream accept failed: {error}");
                    continue;
                }
            };
            spawn(async move {
                loop {
                    let mut request = match connection.recv().await {
                        Ok(request) => request,
                        Err(HttpServeError::ConnectionClosed) => break,
                        Err(error) => {
                            warn!("http-demo: stream receive failed: {error}");
                            break;
                        }
                    };
                    if request.send_head(200, Vec::new()).await.is_err() {
                        break;
                    }
                    for chunk in [
                        b"streamed-".as_slice(),
                        b"in-".as_slice(),
                        b"chunks".as_slice(),
                    ] {
                        if request.send_chunk(chunk.to_vec()).await.is_err() {
                            return;
                        }
                    }
                    if request.finish().await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    loop {
        let mut connection = match server.accept().await {
            Ok(connection) => connection,
            Err(HttpServeError::ConnectionClosed) => {
                warn!("http-demo: connector closed the session");
                continue;
            }
            Err(error) => {
                warn!("http-demo: accept failed: {error}");
                continue;
            }
        };

        spawn(async move {
            loop {
                let request = match connection.recv().await {
                    Ok(request) => request,
                    Err(HttpServeError::ConnectionClosed) => break,
                    Err(error) => {
                        warn!("http-demo: receive failed: {error}");
                        break;
                    }
                };
                let payload = match request.payload() {
                    Ok(payload) => payload,
                    Err(error) => {
                        warn!("http-demo: decode failed: {error}");
                        break;
                    }
                };

                // Deterministic echo: the method, path, and body are reflected
                // so the test can prove the request crossed the whole path
                // byte-for-byte.
                let body = format!(
                    "{} {}\n{}",
                    payload.method,
                    payload.uri,
                    String::from_utf8_lossy(&payload.body)
                );
                let response = HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: body.into_bytes(),
                };
                if let Err(error) = request.reply(response).await {
                    warn!("http-demo: reply failed: {error}");
                    break;
                }
            }
        });
    }
}
