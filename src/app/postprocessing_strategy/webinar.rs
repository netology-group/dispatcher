use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde_derive::Serialize;
use serde_json::Value as JsonValue;
use sqlx::Acquire;
use svc_agent::mqtt::{
    IntoPublishableMessage, OutgoingEvent, OutgoingEventProperties, ShortTermTimingProperties,
};
use uuid::Uuid;

use crate::app::AppContext;
use crate::clients::event::RoomAdjustResult;
use crate::db::class::Object as Class;
use crate::{
    clients::tq::{Task as TqTask, TranscodeStreamToHlsSuccess},
    db::recording::RecordingInsertQuery,
};

use super::{MjrDumpsUploadResult, TranscodeSuccess, UploadedStream};

// TODO: make configurable for each audience.
const PREROLL_OFFSET: i64 = 4018;

pub(super) struct WebinarPostprocessingStrategy {
    ctx: Arc<dyn AppContext>,
    webinar: Class,
}

impl WebinarPostprocessingStrategy {
    pub(super) fn new(ctx: Arc<dyn AppContext>, webinar: Class) -> Self {
        Self { ctx, webinar }
    }
}

#[async_trait]
impl super::PostprocessingStrategy for WebinarPostprocessingStrategy {
    async fn handle_mjr_dumps_upload(&self, rtcs: Vec<MjrDumpsUploadResult>) -> Result<()> {
        let mut ready_dumps = super::shared_helpers::extract_ready_dumps(rtcs)?;
        if ready_dumps.len() != 1 {
            return Err(anyhow!("Expected exactly 1 dump"));
        }
        let dump = ready_dumps.pop().unwrap();
        {
            let mut conn = self.ctx.get_conn().await?;
            RecordingInsertQuery::new(self.webinar.id(), dump.id, dump.created_by)
                .execute(&mut conn)
                .await?;
        }
        self.ctx
            .tq_client()
            .create_task(
                &self.webinar,
                TqTask::ConvertMjrDumpsToStream {
                    mjr_dumps_uris: dump.mjr_dumps_uris,
                    stream_uri: dump.uri,
                    stream_id: dump.id,
                },
            )
            .await
            .context("Failed to set mjr dumps convert task")?;
        Ok(())
    }

    async fn handle_adjust(&self, room_adjust_result: RoomAdjustResult) -> Result<()> {
        match room_adjust_result {
            RoomAdjustResult::Success {
                original_room_id,
                modified_room_id,
                modified_segments,
            } => {
                let recording = {
                    let mut conn = self.ctx.get_conn().await?;

                    let mut txn = conn
                        .begin()
                        .await
                        .context("Failed to begin sqlx db transaction")?;

                    let q = crate::db::class::UpdateQuery::new(
                        self.webinar.id(),
                        original_room_id,
                        modified_room_id,
                    );

                    q.execute(&mut txn).await?;

                    let q = crate::db::recording::AdjustWebinarUpdateQuery::new(
                        self.webinar.id(),
                        modified_segments.clone(),
                    );

                    let recording = q.execute(&mut txn).await?;
                    txn.commit().await?;
                    recording
                };
                self.ctx
                    .event_client()
                    .dump_room(modified_room_id)
                    .await
                    .context("Dump room event failed")?;
                self.ctx
                    .tq_client()
                    .create_task(
                        &self.webinar,
                        TqTask::TranscodeStreamToHls {
                            stream_id: recording.rtc_id(),
                            stream_uri: recording
                                .stream_uri()
                                .ok_or_else(|| {
                                    anyhow!(
                                        "Missing stream_uri in adjust for {}",
                                        recording.rtc_id()
                                    )
                                })?
                                .clone(),
                            event_room_id: Some(modified_room_id),
                            segments: Some(modified_segments),
                        },
                    )
                    .await
                    .context("TqClient create task failed")
            }
            RoomAdjustResult::Error { error } => {
                bail!("Adjust failed, err = {:?}", error);
            }
        }
    }

    async fn handle_transcoding_completion(
        &self,
        completion_result: TranscodeSuccess,
    ) -> Result<()> {
        match completion_result {
            TranscodeSuccess::TranscodeStreamToHls(TranscodeStreamToHlsSuccess {
                stream_duration,
                stream_id,
                stream_uri,
                event_room_id,
            }) => {
                let stream_duration = stream_duration.parse::<f64>()?.round() as u64;

                {
                    let mut conn = self.ctx.get_conn().await?;

                    crate::db::recording::TranscodingUpdateQuery::new(self.webinar.id())
                        .execute(&mut conn)
                        .await?;
                }

                let timing = ShortTermTimingProperties::new(Utc::now());
                let props = OutgoingEventProperties::new("webinar.ready", timing);
                let path = format!("audiences/{}/events", self.webinar.audience());

                let payload = WebinarReady {
                    tags: self.webinar.tags().map(ToOwned::to_owned),
                    stream_duration,
                    stream_uri,
                    stream_id,
                    status: "success",
                    scope: self.webinar.scope().to_owned(),
                    id: self.webinar.id(),
                    event_room_id,
                };

                let event = OutgoingEvent::broadcast(payload, props, &path);
                let boxed_event = Box::new(event) as Box<dyn IntoPublishableMessage + Send>;

                self.ctx
                    .publisher()
                    .publish(boxed_event)
                    .context("Failed to publish webinar.ready event")
            }
            TranscodeSuccess::TranscodeMinigroupToHls(result) => {
                bail!(
                    "Got transcoding success for an unexpected tq template; expected transcode-stream-to-hls for a webinar, id = {}, result = {:?}",
                    self.webinar.id(),
                    result,
                );
            }
        }
    }

    async fn handle_stream_upload(&self, stream: UploadedStream) -> Result<()> {
        let rtc = {
            let mut conn = self.ctx.get_conn().await?;

            crate::db::recording::StreamUploadUpdateQuery::new(
                self.webinar.id(),
                stream.id,
                stream.segments,
                stream.uri,
                stream.started_at,
            )
            .execute(&mut conn)
            .await?
        };

        self.ctx
            .event_client()
            .adjust_room(
                self.webinar.event_room_id(),
                rtc.started_at()
                    .ok_or_else(|| anyhow!("Missing started at after upload"))?,
                rtc.segments()
                    .ok_or_else(|| anyhow!("Missing segments after upload"))?
                    .clone(),
                PREROLL_OFFSET,
            )
            .await
            .context("Failed to adjust room")
    }
}

#[derive(Serialize)]
struct WebinarReady {
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<JsonValue>,
    status: &'static str,
    stream_duration: u64,
    stream_id: Uuid,
    stream_uri: String,
    scope: String,
    id: Uuid,
    event_room_id: Uuid,
}
