use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_derive::Deserialize;
use svc_agent::AgentId;
use uuid::Uuid;

use crate::{app::AppContext, clients::tq::TranscodeMinigroupToHlsSuccess};
use crate::{
    clients::tq::TranscodeStreamToHlsSuccess,
    db::class::{ClassType, Object as Class},
};
use crate::{
    clients::{event::RoomAdjustResult, tq::ConvertMjrDumpsToStreamSuccess},
    db::recording::Segments,
};

use minigroup::MinigroupPostprocessingStrategy;
use webinar::WebinarPostprocessingStrategy;

////////////////////////////////////////////////////////////////////////////////

pub(crate) fn get(
    ctx: Arc<dyn AppContext>,
    class: Class,
) -> Result<Box<dyn PostprocessingStrategy + Send + Sync>> {
    match class.kind() {
        ClassType::P2P => bail!("Postprocessing for a p2p is not available"),
        ClassType::Minigroup => Ok(Box::new(MinigroupPostprocessingStrategy::new(ctx, class))),
        ClassType::Webinar => Ok(Box::new(WebinarPostprocessingStrategy::new(ctx, class))),
    }
}

#[async_trait]
pub(crate) trait PostprocessingStrategy {
    async fn handle_mjr_dumps_upload(&self, rtcs: Vec<MjrDumpsUploadResult>) -> Result<()>;
    async fn handle_stream_upload(&self, stream: UploadedStream) -> Result<()>;
    async fn handle_adjust(&self, room_adjust_result: RoomAdjustResult) -> Result<()>;

    async fn handle_transcoding_completion(
        &self,
        completion_result: TranscodeSuccess,
    ) -> Result<()>;
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Deserialize)]
#[serde(tag = "status")]
#[serde(rename_all = "lowercase")]
pub(crate) enum MjrDumpsUploadResult {
    Ready(MjrDumpsUploadReadyData),
    Missing(MjrDumpsUploadMissingData),
}

#[derive(Debug, Deserialize)]
pub(crate) struct MjrDumpsUploadReadyData {
    pub(self) id: Uuid,
    pub(self) created_by: AgentId,
    pub(self) uri: String,
    pub(self) mjr_dumps_uris: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MjrDumpsUploadMissingData {
    pub(self) id: Uuid,
}

#[derive(Debug)]
pub enum TranscodeSuccess {
    TranscodeStreamToHls(TranscodeStreamToHlsSuccess),
    TranscodeMinigroupToHls(TranscodeMinigroupToHlsSuccess),
}

#[derive(Debug)]
pub struct UploadedStream {
    pub id: Uuid,
    pub uri: String,
    pub started_at: DateTime<Utc>,
    pub segments: Segments,
}

impl UploadedStream {
    pub fn from_convert_result(result: &ConvertMjrDumpsToStreamSuccess) -> Result<Self> {
        let (started_at, segments) = shared_helpers::parse_segments(&result.segments)?;
        Ok(Self {
            id: result.stream_id,
            uri: result.stream_uri.clone(),
            started_at,
            segments,
        })
    }
}

////////////////////////////////////////////////////////////////////////////////

mod minigroup;
pub(self) mod shared_helpers;
mod webinar;
