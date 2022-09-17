use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::Error;
use futures::TryStreamExt;
use http::{Method, Request, Response, StatusCode};
use hyper::server::conn::Http;
use hyper::service::Service;
use hyper::Body;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tracing::instrument;

use crate::injector::InjectorConfig;
#[cfg(unix)]
use crate::todarpc::TodaRpc;

#[derive(Debug)]
pub struct TodaServer {
    toda_rpc: Arc<TodaRpc>,
    task: Option<JoinHandle<Result<(), Error>>>,
}

impl TodaServer {
    pub fn new(toda_rpc: TodaRpc) -> Self {
        Self {
            toda_rpc: Arc::new(toda_rpc),
            task: None,
        }
    }

    pub fn serve_interactive(&mut self, interactive_path: PathBuf) {
        let toda_rpc = self.toda_rpc.clone();
        self.task = Some(tokio::task::spawn(async move {
            tracing::info!("TodaServer listener try binding {:?}", interactive_path);
            let unix_listener = UnixListener::bind(interactive_path).unwrap();

            loop {
                let mut service = TodaService(toda_rpc.clone());
                match (unix_listener).accept().await {
                    Ok((stream, addr)) => {
                        tokio::task::spawn(async move {
                            let http = Http::new();
                            let conn = http.serve_connection(stream, &mut service);
                            if let Err(e) = conn.await {
                                tracing::error!(
                                    "error : http.serve_connection to {:?} failed, error: {:?}",
                                    addr,
                                    e
                                );
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("error: accept connection failed");
                        return Err(anyhow::anyhow!("{}", e));
                    }
                }
            }
        }));
    }
}

pub struct TodaService(Arc<TodaRpc>);

impl TodaService {
    async fn read_config(request: Request<Body>) -> anyhow::Result<Vec<InjectorConfig>> {
        let request_data: Vec<u8> = request
            .into_body()
            .try_fold(vec![], |mut data, seg| {
                data.extend(seg);
                futures::future::ok(data)
            })
            .await?;
        let raw_config: Vec<InjectorConfig> = serde_json::from_slice(&request_data)?;

        Ok(raw_config)
    }

    #[instrument]
    async fn handle(toda_rpc: &TodaRpc, request: Request<Body>) -> anyhow::Result<Response<Body>> {
        let mut response = Response::new(Body::empty());
        if request.method() != Method::PUT {
            *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
            return Ok(response);
        }
        *response.status_mut() = StatusCode::OK;

        match request.uri().path() {
            "/get_status" => match toda_rpc.get_status() {
                Err(err) => {
                    *response.body_mut() = err.to_string().into();
                }
                Ok(res) => {
                    *response.body_mut() = res.into();
                }
            },
            "/update" => {
                let config = match Self::read_config(request).await {
                    Err(e) => {
                        *response.body_mut() = e.to_string().into();
                        *response.status_mut() = StatusCode::BAD_REQUEST;
                        return Ok(response);
                    }
                    Ok(c) => c,
                };
                match toda_rpc.update(config).await {
                    Ok(res) => {
                        *response.body_mut() = res.into();
                    }
                    Err(err) => {
                        *response.body_mut() = err.to_string().into();
                    }
                }
            }
            _ => {
                *response.status_mut() = StatusCode::NOT_FOUND;
            }
        };

        Ok(response)
    }
}

impl Service<Request<Body>> for TodaService {
    type Response = Response<Body>;
    type Error = anyhow::Error;
    #[allow(clippy::type_complexity)]
    type Future =
        Pin<Box<dyn 'static + Send + Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[inline]
    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let handler = self.0.clone();
        Box::pin(async move { Self::handle(&handler, request).await })
    }
}
