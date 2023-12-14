use std::time::Duration;
use anyhow::Result;
use futures::future::try_join_all;
use uuid::Uuid;
use selium::std::errors::SeliumError;
use crate::helpers::{TestClient, Request, Response};

#[tokio::test]
async fn request_reply_successful() -> Result<()> {
    let client = TestClient::start().await?;
    let _ = client.start_replier(None);

    let mut requestor = client.requestor(None).await?;
    let reply = requestor.request(Request::Ping).await?;

    assert_eq!(reply, Response::Pong);

    Ok(())
}

#[tokio::test]
async fn request_fails_if_exceeds_timeout() -> Result<()> {
    let client = TestClient::start().await?;
    let _ = client.start_replier(Some(Duration::from_secs(3)));

    let mut requestor = client.requestor(Some(Duration::from_secs(2))).await?;
    let reply = requestor.request(Request::Ping).await;

    assert!(matches!(reply, Err(SeliumError::RequestTimeout)));

    Ok(())
}

#[tokio::test]
async fn concurrent_requests_are_routed_successfully() -> Result<()> {
    let client = TestClient::start().await?;
    let _ = client.start_replier(None);

    let requestor = client.requestor(None).await?;
    let mut tasks = vec![];

    for _ in 0..10_000 {
        tasks.push(tokio::spawn({
            let mut requestor = requestor.clone();
            let uuid = Uuid::new_v4().to_string();

            async move {
                let reply = requestor.request(Request::Echo(uuid.clone())).await.unwrap();
                assert_eq!(reply, Response::Echo(uuid));
            }
        }));
    }

    try_join_all(tasks).await?;
    
    Ok(())
}

// #[tokio::test]
// async fn fails_to_bind_multiple_repliers_to_topic() -> Result<()> {
//     let client = TestClient::start().await?;
//
//     let bind_tasks = vec![
//         client.start_replier(None),
//         client.start_replier(None)
//     ];
//
//     let _ = select_all(bind_tasks).await;
//
//     Ok(())
// }
