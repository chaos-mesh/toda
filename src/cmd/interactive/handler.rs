use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::Error;
use futures::TryStreamExt;
use http::{Method, Request, Response, StatusCode};
use hyper::server::conn::{Http};
use hyper::service::Service;
use hyper::Body;
use tokio::sync::Mutex;
use std::thread::JoinHandle;
use tokio::runtime::Runtime;
use tracing::instrument;
use tokio::net::{UnixListener};
#[cfg(unix)]
use std::os::unix::io::{FromRawFd};
use crate::todarpc::TodaRpc;
use crate::injector::{InjectorConfig};

#[derive(Debug)]
pub struct TodaServer {
    toda_rpc: Arc<Mutex<TodaRpc>>,
    task: Option<JoinHandle<Result<(), Error>>>,
}

impl TodaServer {
    pub fn new(toda_rpc: TodaRpc) -> Self {
        Self {
            toda_rpc: Arc::new(Mutex::new(toda_rpc)),
            task: None,
        }
    }

    pub fn serve_interactive(&mut self) {
        let mut service = TodaService(self.toda_rpc.clone());

        self.task = Some(std::thread::spawn( move || {
            Runtime::new()
            .expect("Failed to create Tokio runtime")
            .block_on(async {
                let unix_listener = UnixListener::from_std(unsafe {std::os::unix::net::UnixListener::from_raw_fd(3)}).unwrap();

                loop {
                    match (unix_listener).accept().await {
                        Ok((stream, addr)) => {
    
                            let http = Http::new();
                            let conn = http.serve_connection(stream, &mut service);
                            if let Err(e) = conn.await {
                                tracing::error!(
                                    "error : http.serve_connection to {:?} failed, error: {:?}",
                                    addr,
                                    e
                                );
                                return Err(anyhow::anyhow!("{}",e));
                            }
                        }
                        Err(e) => {
                            tracing::error!("error : accept connection failed");
                            return Err(anyhow::anyhow!("{}", e));
                        }
                    }
                }
            }
        )
        }));
    }
}

pub struct TodaService(Arc<Mutex<TodaRpc>>);

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
    async fn handle(toda_rpc: &mut TodaRpc, request: Request<Body>) -> anyhow::Result<Response<Body>> {

        let mut response = Response::new(Body::empty());
        if request.method() != Method::PUT {
            *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
            return Ok(response);
        }
        *response.status_mut() = StatusCode::OK;
        
        match request.uri().path() {
            "/get_status" => {
                match toda_rpc.get_status() {
                    Err(err) => {
                        *response.body_mut() = err.to_string().into();
                    } 
                    Ok(res) => {
                        *response.body_mut() = res.into();
                    }
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
                match toda_rpc.update(config) {
                    Ok(res) => {
                        *response.body_mut() = res.into();
                    }
                    Err(err) => {
                        *response.body_mut() = err.to_string().into();
                    } 
                }
            },
            _ => {
                *response.status_mut() = StatusCode::NOT_FOUND;
            },
        };

        return Ok(response)

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
        Box::pin(async move { Self::handle(&mut *handler.lock().await, request).await })
    }
}

