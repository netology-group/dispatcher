use slog::{error, info, o, warn};
use tide::{http::Method, Middleware, Next, Request};

use crate::LOG;

#[derive(Debug, Default, Clone)]
pub struct LogMiddleware {}

impl LogMiddleware {
    pub fn new() -> Self {
        Self {}
    }

    /// Log a request and a response.
    async fn log<'a, State: Clone + Send + Sync + 'static>(
        &'a self,
        mut req: Request<State>,
        next: Next<'a, State>,
    ) -> tide::Result {
        let path = req.url().path().to_owned();
        let method = req.method().to_string();
        let start = std::time::Instant::now();
        let body = if req.method() != Method::Get {
            let body = req.body_string().await?;
            req.set_body(body.clone());
            Some(body)
        } else {
            None
        };
        let response = next.run(req).await;
        let status = response.status();
        // TODO: once https://github.com/slog-rs/slog/issues/248 is fixed
        // calls to format! and method .to_string() conversion can be replaced with
        // ?start.elapsed() and %method in o!() invocation
        let logger = LOG.new(o!(
            "method" => method,
            "path" => path,
            "status" => status as u16,
            "duration" => format!("{:?}", start.elapsed()),
        ));

        let logger = if let Some(body) = body {
            logger.new(o!("body" => body))
        } else {
            logger
        };

        if status.is_server_error() {
            if let Some(error) = response.error() {
                error!(logger, "HTTP response";
                    "message" => format!("{:?}", error),
                    "error_type" => error.type_name(),
                );
            } else {
                error!(logger, "HTTP response");
            }
        } else if status.is_client_error() {
            if let Some(error) = response.error() {
                warn!(logger, "HTTP response";
                    "message" => format!("{:?}", error),
                    "error_type" => error.type_name(),
                );
            } else {
                warn!(logger, "HTTP response");
            }
        } else {
            info!(logger, "HTTP response");
        }
        Ok(response)
    }
}

#[async_trait::async_trait]
impl<State: Clone + Send + Sync + 'static> Middleware<State> for LogMiddleware {
    async fn handle(&self, req: Request<State>, next: Next<'_, State>) -> tide::Result {
        self.log(req, next).await
    }
}
