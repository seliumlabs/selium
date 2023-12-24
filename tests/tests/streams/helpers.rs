use anyhow::Result;
use clap::Parser;
use selium::prelude::*;
use selium::std::codecs::BincodeCodec;
use selium::std::errors::SeliumError;
use selium::{request_reply::Requestor, Client};
use selium_server::args::UserArgs;
use selium_server::server::Server;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

// Allow the operating system to assign a free port
const SERVER_ADDR: &str = "127.0.0.1:0";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Request {
    Ping,
    Echo(String),
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub enum Response {
    Pong,
    Echo(String),
}

type Req = Requestor<BincodeCodec<Request>, BincodeCodec<Response>, Request, Response>;

pub struct TestClient {
    client: Client,
}

impl TestClient {
    pub async fn start() -> Result<Self> {
        let server_addr = start_server()?;

        let client = selium::custom()
            .keep_alive(5_000)?
            .endpoint(&server_addr.to_string())
            .with_certificate_authority("../certs/client/ca.der")?
            .with_cert_and_key(
                "../certs/client/localhost.der",
                "../certs/client/localhost.key.der",
            )?
            .connect()
            .await?;

        Ok(Self { client })
    }

    pub fn start_replier(
        &self,
        delay: Option<Duration>,
    ) -> tokio::task::JoinHandle<Result<(), SeliumError>> {
        tokio::spawn({
            let client = self.client.clone();

            async move {
                let replier = client
                    .replier("/test/endpoint")
                    .with_request_decoder(BincodeCodec::default())
                    .with_reply_encoder(BincodeCodec::default())
                    .with_handler(|req| async move {
                        if let Some(delay) = delay {
                            tokio::time::sleep(delay).await;
                        }

                        handler(req).await
                    })
                    .open()
                    .await
                    .unwrap();

                replier.listen(|_| false).await
            }
        })
    }

    pub async fn requestor(&self, timeout: Option<Duration>) -> Result<Req> {
        let mut builder = self
            .client
            .requestor("/test/endpoint")
            .with_request_encoder(BincodeCodec::default())
            .with_reply_decoder(BincodeCodec::default());

        if let Some(timeout) = timeout {
            builder = builder.with_request_timeout(timeout)?;
        }

        let requestor = builder.open().await?;

        Ok(requestor)
    }
}

async fn handler(req: Request) -> Result<Response> {
    let res = match req {
        Request::Ping => Response::Pong,
        Request::Echo(msg) => Response::Echo(msg),
    };

    Ok(res)
}

pub fn start_server() -> Result<SocketAddr> {
    let args = UserArgs::parse_from([
        "",
        "--bind-addr",
        SERVER_ADDR,
        "--cert",
        "../certs/server/localhost.der",
        "--key",
        "../certs/server/localhost.key.der",
        "--ca",
        "../certs/server/ca.der",
    ]);

    let server = Server::try_from(args)?;
    let addr = server.addr()?;

    tokio::spawn(async move {
        server.listen().await.expect("Failed to spawn server");
    });

    Ok(addr)
}
