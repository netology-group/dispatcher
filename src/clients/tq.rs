use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::AsyncReadExt;
use isahc::config::Configurable;
#[cfg(test)]
use mockall::{automock, predicate::*};
use serde_derive::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use super::ClientError;
use crate::db::recording::Segments;

const PRIORITY: &str = "normal";

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Task {
    TranscodeStreamToHls {
        stream_id: Uuid,
        stream_uri: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        event_room_id: Option<Uuid>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(serialize_with = "crate::db::recording::serde::segments_option")]
        segments: Option<Segments>,
    },
    TranscodeMinigroupToHls {
        streams: Vec<TranscodeMinigroupToHlsStream>,
        host_stream_id: Uuid,
    },
    ConvertMjrDumpsToStream {
        mjr_dumps_uris: Vec<String>,
        stream_uri: String,
        stream_id: Uuid,
    },
}

impl Task {
    fn template(&self) -> &'static str {
        match self {
            Self::TranscodeStreamToHls { .. } => "transcode-stream-to-hls",
            Self::TranscodeMinigroupToHls { .. } => "transcode-minigroup-to-hls",
            Self::ConvertMjrDumpsToStream { .. } => "convert-mjr-dumps-to-stream",
        }
    }
}

#[derive(Debug, PartialEq, Serialize)]
pub struct TranscodeMinigroupToHlsStream {
    id: Uuid,
    uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "crate::db::recording::serde::segments_option")]
    segments: Option<Segments>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "crate::db::recording::serde::segments_option")]
    pin_segments: Option<Segments>,
}

impl TranscodeMinigroupToHlsStream {
    pub fn new(id: Uuid, uri: String) -> Self {
        Self {
            id,
            uri,
            offset: None,
            segments: None,
            pin_segments: None,
        }
    }

    pub fn offset(self, offset: u64) -> Self {
        Self {
            offset: Some(offset),
            ..self
        }
    }

    pub fn segments(self, segments: Segments) -> Self {
        Self {
            segments: Some(segments),
            ..self
        }
    }

    pub fn pin_segments(self, pin_segments: Segments) -> Self {
        Self {
            pin_segments: Some(pin_segments),
            ..self
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TaskComplete {
    pub tags: Option<JsonValue>,
    #[serde(flatten)]
    pub result: TaskCompleteResult,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status")]
pub enum TaskCompleteResult {
    #[serde(rename = "success")]
    Success(TaskCompleteSuccess),
    #[serde(rename = "failure")]
    Failure { error: Option<JsonValue> },
}

impl From<TaskComplete> for TaskCompleteResult {
    fn from(task_complete: TaskComplete) -> Self {
        task_complete.result
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "template")]
pub enum TaskCompleteSuccess {
    #[serde(rename = "transcode-stream-to-hls")]
    TranscodeStreamToHls(TranscodeStreamToHlsSuccess),
    #[serde(rename = "transcode-minigroup-to-hls")]
    TranscodeMinigroupToHls(TranscodeMinigroupToHlsSuccess),
    #[serde(rename = "convert-mjr-dumps-to-stream")]
    ConvertMjrDumpsToStream(ConvertMjrDumpsToStreamSuccess),
}

#[derive(Debug, Deserialize)]
pub struct ConvertMjrDumpsToStreamSuccess {
    pub stream_id: Uuid,
    pub stream_uri: String,
    pub segments: String,
}

#[derive(Debug, Deserialize)]
pub struct TranscodeStreamToHlsSuccess {
    pub stream_id: Uuid,
    pub stream_uri: String,
    pub stream_duration: String,
    pub event_room_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct TranscodeMinigroupToHlsSuccess {
    pub recording_duration: String,
}

////////////////////////////////////////////////////////////////////////////////

#[cfg_attr(test, automock)]
#[async_trait]
pub trait TqClient: Sync + Send {
    async fn create_task(
        &self,
        class: &crate::db::class::Object,
        task: Task,
    ) -> Result<(), ClientError>;
}

pub struct HttpTqClient {
    client: isahc::HttpClient,
    base_url: url::Url,
}

impl HttpTqClient {
    pub fn new(base_url: String, token: String, timeout: u64) -> Self {
        let base_url = url::Url::parse(&base_url).expect("Failed to convert HttpTqClient base url");

        let client = isahc::HttpClient::builder()
            .timeout(Duration::from_secs(timeout))
            .default_header(
                http::header::AUTHORIZATION.as_str(),
                format!("Bearer {}", token),
            )
            .default_header(http::header::CONTENT_TYPE.as_str(), "application/json")
            .default_header(
                http::header::USER_AGENT.as_str(),
                format!("dispatcher-{}", crate::APP_VERSION),
            )
            .build()
            .expect("Failed to build HttpTqClient");

        Self { client, base_url }
    }
}

#[derive(Serialize)]
struct TaskPayload {
    audience: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<JsonValue>,
    priority: String,
    template: String,
    bindings: Task,
}

#[async_trait]
impl TqClient for HttpTqClient {
    async fn create_task(
        &self,
        class: &crate::db::class::Object,
        task: Task,
    ) -> Result<(), ClientError> {
        let route = format!(
            "/api/v1/audiences/{}/tasks/{}",
            class.audience(),
            task.template().to_owned() + class.scope()
        );

        let task = TaskPayload {
            audience: class.audience().to_owned(),
            tags: class
                .tags()
                .map(ToOwned::to_owned)
                .or_else(|| Some(json!({"scope": class.scope().to_owned()}))),
            priority: PRIORITY.into(),
            template: task.template().into(),
            bindings: task,
        };

        let url = self.base_url.join(&route).map_err(|e| {
            ClientError::HttpError(format!(
                "Failed to join base_url with route, base_url = {}, route = {}, err = {}",
                self.base_url, route, e
            ))
        })?;

        let json =
            serde_json::to_string(&task).map_err(|e| ClientError::PayloadError(e.to_string()))?;
        let mut resp = self
            .client
            .post_async(url.as_str(), json)
            .await
            .map_err(|e| ClientError::HttpError(e.to_string()))?;
        if resp.status() == http::StatusCode::OK {
            Ok(())
        } else {
            let mut body = String::new();
            let e = if let Err(e) = resp.body_mut().read_to_string(&mut body).await {
                format!(
                    "Failed to create tq task and read response body, status = {:?}, response body read = {:?}, error = {:?}",
                    resp.status(),
                    body,
                    e
                )
            } else {
                format!(
                    "Failed to create tq task, status = {:?}, response = {:?}",
                    resp.status(),
                    body
                )
            };
            Err(ClientError::PayloadError(e))
        }
    }
}
