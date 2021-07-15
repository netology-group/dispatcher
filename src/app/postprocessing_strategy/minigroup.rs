use std::ops::Bound;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::{postgres::PgConnection, Acquire};
use svc_agent::{
    mqtt::{
        IntoPublishableMessage, OutgoingEvent, OutgoingEventProperties, ShortTermTimingProperties,
    },
    AgentId,
};
use uuid::Uuid;

use crate::app::AppContext;
use crate::clients::event::{Event, EventData, RoomAdjustResult};
use crate::db::class::Object as Class;
use crate::db::recording::BoundedOffsetTuples;
use crate::{
    clients::tq::{Task as TqTask, TranscodeMinigroupToHlsStream, TranscodeMinigroupToHlsSuccess},
    db::recording::Segments,
};

use super::{
    shared_helpers, MjrDumpsUploadReadyData, MjrDumpsUploadResult, TranscodeSuccess, UploadedStream,
};

const NS_IN_MS: i64 = 1000000;
const PIN_EVENT_TYPE: &str = "pin";
const HOST_EVENT_TYPE: &str = "host";
// TODO: make configurable for each audience.
const PREROLL_OFFSET: i64 = 4018;

pub(super) struct MinigroupPostprocessingStrategy {
    ctx: Arc<dyn AppContext>,
    minigroup: Class,
}

impl MinigroupPostprocessingStrategy {
    pub(super) fn new(ctx: Arc<dyn AppContext>, minigroup: Class) -> Self {
        Self { ctx, minigroup }
    }
}

#[async_trait]
impl super::PostprocessingStrategy for MinigroupPostprocessingStrategy {
    async fn handle_stream_upload(&self, stream: UploadedStream) -> Result<()> {
        let recordings = {
            let mut conn = self.ctx.get_conn().await?;
            crate::db::recording::StreamUploadUpdateQuery::new(
                self.minigroup.id(),
                stream.id,
                stream.segments,
                stream.uri,
                stream.started_at,
            )
            .execute(&mut conn)
            .await?;
            crate::db::recording::RecordingListQuery::new(self.minigroup.id())
                .execute(&mut conn)
                .await?
        };
        let ready_recordings = recordings
            .iter()
            .filter_map(|recording| ReadyRecording::from_db_object(recording))
            .collect::<Vec<_>>();
        if recordings.len() != ready_recordings.len() {
            return Ok(());
        }

        call_adjust(
            self.ctx.clone(),
            self.minigroup.event_room_id(),
            ready_recordings,
        )
        .await?;
        Ok(())
    }

    async fn handle_adjust(&self, room_adjust_result: RoomAdjustResult) -> Result<()> {
        match room_adjust_result {
            RoomAdjustResult::Success {
                original_room_id,
                modified_room_id,
                ..
            } => {
                // Save adjust results to the DB and fetch recordings.
                let recordings = {
                    let mut conn = self.ctx.get_conn().await?;

                    let mut txn = conn
                        .begin()
                        .await
                        .context("Failed to begin sqlx db transaction")?;

                    let q = crate::db::class::UpdateQuery::new(
                        self.minigroup.id(),
                        original_room_id,
                        modified_room_id,
                    );

                    q.execute(&mut txn).await?;

                    let recordings =
                        crate::db::recording::AdjustMinigroupUpdateQuery::new(self.minigroup.id())
                            .execute(&mut txn)
                            .await?;

                    txn.commit().await?;
                    recordings
                        .into_iter()
                        .map(|recording| ReadyRecording::from_db_object(&recording))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| anyhow!("Not all recordings are ready"))?
                };

                // Find the earliest recording.
                let earliest_recording = recordings
                    .iter()
                    .min_by(|a, b| a.started_at.cmp(&b.started_at))
                    .ok_or_else(|| anyhow!("No recordings"))?;

                // Fetch event room opening time for events' offset calculation.
                let modified_event_room = self
                    .ctx
                    .event_client()
                    .read_room(modified_room_id)
                    .await
                    .context("Failed to read modified event room")?;

                let modified_event_room_opened_at = match modified_event_room.time {
                    (Bound::Included(opened_at), _) => opened_at,
                    _ => bail!("Wrong event room opening time"),
                };

                // Fetch pin events for building pin segments.
                let pin_events = self
                    .ctx
                    .event_client()
                    .list_events(modified_room_id, PIN_EVENT_TYPE)
                    .await
                    .context("Failed to get pin events for room")?;

                // Build streams for template bindings.
                let streams = recordings
                    .iter()
                    .map(|recording| {
                        let event_room_offset =
                            recording.started_at - modified_event_room_opened_at;

                        let recording_offset = recording.started_at - earliest_recording.started_at;

                        build_stream(recording, &pin_events, event_room_offset, recording_offset)
                    })
                    .collect::<Vec<_>>();

                // Find host stream id.
                let host = match self.find_host(modified_event_room.id).await? {
                    // Host has not been set, skip transcoding.
                    None => return Ok(()),
                    Some(agent_id) => agent_id,
                };

                let maybe_host_recording = recordings
                    .iter()
                    .find(|recording| recording.created_by == host);

                let host_stream_id = match maybe_host_recording {
                    // Host has been set but there's no recording, skip transcoding.
                    None => return Ok(()),
                    Some(recording) => recording.rtc_id,
                };

                // Create a tq task.
                let task = TqTask::TranscodeMinigroupToHls {
                    streams,
                    host_stream_id,
                };

                self.ctx
                    .tq_client()
                    .create_task(&self.minigroup, task)
                    .await
                    .context("TqClient create task failed")
            }
            RoomAdjustResult::Error { error } => {
                bail!("Adjust failed, err = {:#?}", error);
            }
        }
    }

    async fn handle_transcoding_completion(
        &self,
        completion_result: TranscodeSuccess,
    ) -> Result<()> {
        match completion_result {
            TranscodeSuccess::TranscodeMinigroupToHls(TranscodeMinigroupToHlsSuccess {
                recording_duration,
                ..
            }) => {
                let recording_duration = recording_duration.parse::<f64>()?.round() as u64;

                {
                    let mut conn = self.ctx.get_conn().await?;

                    crate::db::recording::TranscodingUpdateQuery::new(self.minigroup.id())
                        .execute(&mut conn)
                        .await?;
                }

                let timing = ShortTermTimingProperties::new(Utc::now());
                let props = OutgoingEventProperties::new("minigroup.ready", timing);
                let path = format!("audiences/{}/events", self.minigroup.audience());

                let payload = MinigroupReady {
                    id: self.minigroup.id(),
                    scope: self.minigroup.scope().to_owned(),
                    tags: self.minigroup.tags().map(ToOwned::to_owned),
                    status: "success".to_string(),
                    recording_duration,
                };

                let event = OutgoingEvent::broadcast(payload, props, &path);
                let boxed_event = Box::new(event) as Box<dyn IntoPublishableMessage + Send>;

                self.ctx
                    .publisher()
                    .publish(boxed_event)
                    .context("Failed to publish minigroup.ready event")
            }
            TranscodeSuccess::TranscodeStreamToHls(success_result) => {
                bail!(
                    "Got transcoding success for an unexpected tq template; expected transcode-minigroup-to-hls for a minigroup, id = {}, result = {:#?}",
                    self.minigroup.id(),
                    success_result,
                );
            }
        }
    }

    async fn handle_mjr_dumps_upload(&self, dumps: Vec<MjrDumpsUploadResult>) -> Result<()> {
        if dumps.is_empty() {
            bail!("Expected at least 1 RTC");
        }

        let ready_dumps = shared_helpers::extract_ready_dumps(dumps)?;
        {
            let mut conn = self.ctx.get_conn().await?;
            insert_recordings(&mut conn, self.minigroup.id(), &ready_dumps).await?;
        }
        let tq_client = self.ctx.tq_client();
        for dump in ready_dumps {
            tq_client
                .create_task(
                    &self.minigroup,
                    TqTask::ConvertMjrDumpsToStream {
                        mjr_dumps_uris: dump.mjr_dumps_uris,
                        stream_uri: dump.uri,
                        stream_id: dump.id,
                    },
                )
                .await?
        }
        Ok(())
    }
}

impl MinigroupPostprocessingStrategy {
    async fn find_host(&self, event_room_id: Uuid) -> Result<Option<AgentId>> {
        let host_events = self
            .ctx
            .event_client()
            .list_events(event_room_id, HOST_EVENT_TYPE)
            .await
            .context("Failed to get host events for room")?;

        match host_events.first().map(|event| event.data()) {
            None => Ok(None),
            Some(EventData::Host(data)) => Ok(Some(data.agent_id().to_owned())),
            Some(other) => bail!("Got unexpected host event data: {:?}", other),
        }
    }
}

async fn insert_recordings(
    conn: &mut PgConnection,
    class_id: Uuid,
    dumps: &[MjrDumpsUploadReadyData],
) -> Result<()> {
    let mut txn = conn
        .begin()
        .await
        .context("Failed to begin sqlx db transaction")?;

    for dump in dumps {
        let q = crate::db::recording::RecordingInsertQuery::new(
            class_id,
            dump.id,
            dump.created_by.to_owned(),
        );

        q.execute(&mut txn).await?;
    }

    txn.commit().await?;
    Ok(())
}

async fn call_adjust(
    ctx: Arc<dyn AppContext>,
    room_id: Uuid,
    recordings: Vec<ReadyRecording>,
) -> Result<()> {
    let started_at = recordings
        .iter()
        .map(|rtc| rtc.started_at)
        .min()
        .ok_or_else(|| anyhow!("Couldn't get min started at"))?;

    let segments = build_adjust_segments(&recordings)?;

    ctx.event_client()
        .adjust_room(room_id, started_at, segments, PREROLL_OFFSET)
        .await
        .map_err(|err| anyhow!("Failed to adjust room, id = {}: {}", room_id, err))?;

    Ok(())
}

fn build_adjust_segments(rtcs: &[ReadyRecording]) -> Result<Segments> {
    let mut maybe_min_start: Option<i64> = None;
    let mut maybe_max_stop: Option<i64> = None;

    for rtc in rtcs.iter() {
        let segments: BoundedOffsetTuples = rtc.segments.clone().into();

        if let Some((Bound::Included(start), _)) = segments.first() {
            if let Some(min_start) = maybe_min_start {
                if *start < min_start {
                    maybe_min_start = Some(*start);
                }
            } else {
                maybe_min_start = Some(*start);
            }
        }

        if let Some((_, Bound::Excluded(stop))) = segments.last() {
            if let Some(max_stop) = maybe_max_stop {
                if *stop > max_stop {
                    maybe_max_stop = Some(*stop);
                }
            } else {
                maybe_max_stop = Some(*stop);
            }
        }
    }

    if let (Some(start), Some(stop)) = (maybe_min_start, maybe_max_stop) {
        Ok(vec![(Bound::Included(start), Bound::Excluded(stop))].into())
    } else {
        bail!("Couldn't find min start & max stop in segments");
    }
}

fn build_stream(
    recording: &ReadyRecording,
    pin_events: &[Event],
    event_room_offset: Duration,
    recording_offset: Duration,
) -> TranscodeMinigroupToHlsStream {
    let event_room_offset = event_room_offset.num_milliseconds();
    let mut pin_segments = vec![];
    let mut pin_start = None;

    for event in pin_events {
        if let EventData::Pin(data) = event.data() {
            // Shift from the event room's dimension to the recording's dimension.
            let occurred_at = event.occurred_at() as i64 / NS_IN_MS - event_room_offset;

            if data
                .agent_id()
                .map(|aid| aid == &recording.created_by)
                .unwrap_or(false)
                && pin_start.is_none()
            {
                // Stream has got pinned.
                pin_start = Some(occurred_at);
            } else if let Some(pinned_at) = pin_start {
                // Stream has got unpinned.
                pin_segments.push((Bound::Included(pinned_at), Bound::Excluded(occurred_at)));
                pin_start = None;
            }
        }
    }

    // If the stream hasn't got unpinned since some moment then add a pin segment to the end
    // of the recording to keep it pinned.
    if let Some(start) = pin_start {
        let recording_segments: BoundedOffsetTuples = recording.segments.clone().into();

        if let Some((_, Bound::Excluded(recording_end))) = recording_segments.last() {
            pin_segments.push((Bound::Included(start), Bound::Excluded(*recording_end)));
        }
    }

    TranscodeMinigroupToHlsStream::new(recording.rtc_id, recording.stream_uri.clone())
        .offset(recording_offset.num_milliseconds() as u64)
        .segments(recording.segments.clone())
        .pin_segments(pin_segments.into())
}

#[derive(Debug)]
struct ReadyRecording {
    rtc_id: Uuid,
    stream_uri: String,
    segments: Segments,
    started_at: DateTime<Utc>,
    created_by: AgentId,
}

impl ReadyRecording {
    fn from_db_object(recording: &crate::db::recording::Object) -> Option<Self> {
        Some(Self {
            rtc_id: recording.rtc_id(),
            stream_uri: recording.stream_uri().cloned()?,
            segments: recording.segments().cloned()?,
            started_at: recording.started_at()?,
            created_by: recording.created_by().clone(),
        })
    }
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct MinigroupReady {
    id: Uuid,
    scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<JsonValue>,
    status: String,
    recording_duration: u64,
}

// ////////////////////////////////////////////////////////////////////////////////

#[cfg(test)]
mod tests {
    mod handle_upload {
        use std::ops::Bound;
        use std::sync::Arc;

        use chrono::{DateTime, Duration, Utc};
        use uuid::Uuid;

        use crate::app::AppContext;
        use crate::clients::event::test_helpers::EventBuilder;
        use crate::clients::event::{EventData, HostEventData};
        use crate::db::recording::{RecordingListQuery, Segments};
        use crate::test_helpers::{prelude::*, shared_helpers::random_string};

        use super::super::super::{
            MjrDumpsUploadReadyData, MjrDumpsUploadResult, PostprocessingStrategy,
        };
        use super::super::*;

        #[async_std::test]
        async fn handle_upload_stream() {
            let now = Utc::now();
            let mut state = TestState::new(TestAuthz::new()).await;
            let conference_room_id = Uuid::new_v4();
            let event_room_id = Uuid::new_v4();
            let agent1 = TestAgent::new("web", "user1", USR_AUDIENCE);
            let agent1_clone = agent1.clone();
            let agent2 = TestAgent::new("web", "user2", USR_AUDIENCE);

            // Insert a minigroup.
            let minigroup = {
                let mut conn = state.get_conn().await.expect("Failed to get conn");

                let time = (
                    Bound::Included(now - Duration::hours(1)),
                    Bound::Excluded(now - Duration::minutes(10)),
                );

                let minigroup_scope = format!("minigroup-{}", random_string());

                factory::Minigroup::new(
                    minigroup_scope,
                    USR_AUDIENCE.to_string(),
                    time.into(),
                    conference_room_id,
                    event_room_id,
                )
                .insert(&mut conn)
                .await
            };
            let rtc1_id = Uuid::new_v4();
            let rtc2_id = Uuid::new_v4();

            let minigroup_id = minigroup.id();

            {
                let mut conn = state.get_conn().await.expect("Failed to get conn");
                let agent1 = TestAgent::new("web", "user1", USR_AUDIENCE);

                factory::Recording::new(minigroup_id, rtc1_id, agent1.agent_id().clone())
                    .insert(&mut conn)
                    .await;

                let agent2 = TestAgent::new("web", "user2", USR_AUDIENCE);
                factory::Recording::new(minigroup_id, rtc2_id, agent2.agent_id().clone())
                    .insert(&mut conn)
                    .await;
            };

            // Set up event client mock.
            let started_at1: DateTime<Utc> = now - Duration::hours(1);

            let segments1: Segments = vec![
                (Bound::Included(0), Bound::Excluded(1500000)),
                (Bound::Included(1800000), Bound::Excluded(3000000)),
            ]
            .into();

            let expected_segments: Segments =
                vec![(Bound::Included(0), Bound::Excluded(3000000))].into();

            state
                .event_client_mock()
                .expect_list_events()
                .withf(move |room_id: &Uuid, _kind: &str| {
                    assert_eq!(*room_id, event_room_id);
                    true
                })
                .returning(move |_, kind| match kind {
                    HOST_EVENT_TYPE => Ok(vec![EventBuilder::new()
                        .room_id(event_room_id)
                        .set(HOST_EVENT_TYPE.to_string())
                        .data(EventData::Host(HostEventData::new(
                            agent1_clone.agent_id().to_owned(),
                        )))
                        .occurred_at(0)
                        .build()]),
                    other => panic!("Event client mock got unknown kind: {}", other),
                });
            state
                .event_client_mock()
                .expect_adjust_room()
                .withf(
                    move |room_id: &Uuid,
                          started_at: &DateTime<Utc>,
                          segments: &Segments,
                          offset: &i64| {
                        assert_eq!(*room_id, event_room_id);
                        assert_eq!(started_at.timestamp(), started_at1.timestamp());
                        assert_eq!(segments, &expected_segments);
                        assert_eq!(*offset, PREROLL_OFFSET);
                        true
                    },
                )
                .returning(|_, _, _, _| Ok(()));

            // Handle uploading two RTCs.
            let uri1 = "s3://minigroup.origin.dev.example.com/rtc1.webm";

            let stream1 = UploadedStream {
                id: rtc1_id,
                uri: uri1.to_string(),
                started_at: started_at1,
                segments: segments1.clone(),
            };

            let uri2 = "s3://minigroup.origin.dev.example.com/rtc2.webm";
            let started_at2 = now - Duration::minutes(50);
            let segments2: Segments = vec![(Bound::Included(0), Bound::Excluded(2700000))].into();

            let stream2 = UploadedStream {
                id: rtc2_id,
                uri: uri2.to_string(),
                started_at: started_at2,
                segments: segments2.clone(),
            };

            let state = Arc::new(state);

            MinigroupPostprocessingStrategy::new(state.clone(), minigroup.clone())
                .handle_stream_upload(stream1)
                .await
                .expect("Failed to handle upload");

            // Assert recordings in the DB.
            {
                let mut conn = state.get_conn().await.expect("Failed to get conn");

                let ready_items = RecordingListQuery::new(minigroup_id)
                    .execute(&mut conn)
                    .await
                    .expect("Failed to list recordings")
                    .into_iter()
                    .filter_map(|recording| ReadyRecording::from_db_object(&recording))
                    .count();
                assert_eq!(ready_items, 1);
            }

            MinigroupPostprocessingStrategy::new(state.clone(), minigroup)
                .handle_stream_upload(stream2)
                .await
                .expect("Failed to handle upload");

            let recordings = {
                let mut conn = state.get_conn().await.expect("Failed to get conn");

                RecordingListQuery::new(minigroup_id)
                    .execute(&mut conn)
                    .await
                    .expect("Failed to list recordings")
                    .into_iter()
                    .filter_map(|recording| ReadyRecording::from_db_object(&recording))
                    .collect::<Vec<_>>()
            };

            assert_eq!(recordings.len(), 2);

            let recording1 = recordings
                .iter()
                .find(|recording| recording.rtc_id == rtc1_id)
                .expect("Recording 1 not found");

            assert_eq!(&recording1.stream_uri, uri1);
            assert!(datetimes_almost_eq(recording1.started_at, started_at1));
            assert_eq!(&recording1.segments, &segments1);
            assert_eq!(&recording1.created_by, agent1.agent_id());

            let recording2 = recordings
                .iter()
                .find(|recording| recording.rtc_id == rtc2_id)
                .expect("Recording 2 not found");

            assert_eq!(&recording2.stream_uri, uri2);
            assert!(datetimes_almost_eq(
                recording2.started_at,
                now - Duration::minutes(50)
            ));
            assert_eq!(&recording2.segments, &segments2);
            assert_eq!(&recording2.created_by, agent2.agent_id());
        }

        #[async_std::test]
        async fn handle_upload_mjr() {
            let now = Utc::now();
            let mut state = TestState::new(TestAuthz::new()).await;
            let conference_room_id = Uuid::new_v4();
            let event_room_id = Uuid::new_v4();

            // Insert a minigroup.
            let minigroup = {
                let mut conn = state.get_conn().await.expect("Failed to get conn");

                let time = (
                    Bound::Included(now - Duration::hours(1)),
                    Bound::Excluded(now - Duration::minutes(10)),
                );

                let minigroup_scope = format!("minigroup-{}", random_string());

                factory::Minigroup::new(
                    minigroup_scope,
                    USR_AUDIENCE.to_string(),
                    time.into(),
                    conference_room_id,
                    event_room_id,
                )
                .insert(&mut conn)
                .await
            };

            let minigroup_id = minigroup.id();

            let dumps = vec![
                "s3://minigroup.origin.dev.example.com/rtc1.mjr".to_owned(),
                "s3://minigroup.origin.dev.example.com/rtc2.mjr".to_owned(),
            ];

            state
                .tq_client_mock()
                .expect_create_task()
                .times(2)
                .returning(|_, _| Ok(()));

            // Handle uploading two RTCs.
            let rtc1_id = Uuid::new_v4();
            let uri1 = "s3://minigroup.origin.dev.example.com/rtc1.webm";
            let agent1 = TestAgent::new("web", "user1", USR_AUDIENCE);

            let rtc1 = MjrDumpsUploadResult::Ready(MjrDumpsUploadReadyData {
                id: rtc1_id,
                uri: uri1.to_string(),
                created_by: agent1.agent_id().to_owned(),
                mjr_dumps_uris: dumps.clone(),
            });

            let rtc2_id = Uuid::new_v4();
            let uri2 = "s3://minigroup.origin.dev.example.com/rtc2.webm";
            let agent2 = TestAgent::new("web", "user2", USR_AUDIENCE);

            let rtc2 = MjrDumpsUploadResult::Ready(MjrDumpsUploadReadyData {
                id: rtc2_id,
                uri: uri2.to_string(),
                created_by: agent2.agent_id().to_owned(),
                mjr_dumps_uris: dumps.clone(),
            });

            let state = Arc::new(state);

            MinigroupPostprocessingStrategy::new(state.clone(), minigroup)
                .handle_mjr_dumps_upload(vec![rtc1, rtc2])
                .await
                .expect("Failed to handle upload");

            // Assert recordings in the DB.
            let recordings = {
                let mut conn = state.get_conn().await.expect("Failed to get conn");

                RecordingListQuery::new(minigroup_id)
                    .execute(&mut conn)
                    .await
                    .expect("Failed to list recordings")
            };

            assert_eq!(recordings.len(), 2);

            let recording1 = recordings
                .iter()
                .find(|recording| recording.rtc_id() == rtc1_id)
                .expect("Recording 1 not found");

            assert_eq!(recording1.stream_uri(), None);
            assert_eq!(recording1.segments(), None);
            assert_eq!(recording1.created_by(), agent1.agent_id());

            let recording2 = recordings
                .iter()
                .find(|recording| recording.rtc_id() == rtc2_id)
                .expect("Recording 2 not found");

            assert_eq!(recording2.stream_uri(), None);
            assert_eq!(recording2.segments(), None);
            assert_eq!(recording2.created_by(), agent2.agent_id());
        }
    }

    mod handle_adjust {
        use std::ops::Bound;
        use std::sync::Arc;

        use chrono::{Duration, Utc};
        use uuid::Uuid;

        use crate::app::AppContext;
        use crate::clients::event::test_helpers::EventBuilder;
        use crate::clients::event::{EventData, EventRoomResponse, HostEventData, PinEventData};
        use crate::db::class::MinigroupReadQuery;
        use crate::db::recording::{RecordingListQuery, Segments};
        use crate::test_helpers::{prelude::*, shared_helpers::random_string};

        use super::super::super::PostprocessingStrategy;
        use super::super::*;

        #[async_std::test]
        async fn handle_adjust() {
            let now = Utc::now();
            let agent1 = TestAgent::new("web", "user1", USR_AUDIENCE);
            let agent2 = TestAgent::new("web", "user2", USR_AUDIENCE);
            let mut state = TestState::new(TestAuthz::new()).await;
            let event_room_id = Uuid::new_v4();
            let original_event_room_id = Uuid::new_v4();
            let modified_event_room_id = Uuid::new_v4();

            // Insert a minigroup with recordings.
            let (minigroup, recording1, recording2) = {
                let mut conn = state.get_conn().await.expect("Failed to get conn");

                let time = (
                    Bound::Included(now - Duration::hours(1)),
                    Bound::Excluded(now - Duration::minutes(10)),
                );

                let minigroup_scope = format!("minigroup-{}", random_string());

                let minigroup = factory::Minigroup::new(
                    minigroup_scope,
                    USR_AUDIENCE.to_string(),
                    time.into(),
                    Uuid::new_v4(),
                    event_room_id,
                )
                .insert(&mut conn)
                .await;

                let segments1: Segments = vec![
                    (Bound::Included(0), Bound::Excluded(1500000)),
                    (Bound::Included(1800000), Bound::Excluded(3000000)),
                ]
                .into();

                let recording1 = factory::Recording::new(
                    minigroup.id(),
                    Uuid::new_v4(),
                    agent1.agent_id().to_owned(),
                )
                .stream_uri("s3://minigroup.origin.dev.example.com/rtc1.webm".to_string())
                .segments(segments1)
                .started_at(now - Duration::hours(1))
                .insert(&mut conn)
                .await;

                let recording2 = factory::Recording::new(
                    minigroup.id(),
                    Uuid::new_v4(),
                    agent2.agent_id().to_owned(),
                )
                .stream_uri("s3://minigroup.origin.dev.example.com/rtc2.webm".to_string())
                .segments(vec![(Bound::Included(0), Bound::Excluded(2700000))].into())
                .started_at(now - Duration::minutes(50))
                .insert(&mut conn)
                .await;

                (minigroup, recording1, recording2)
            };

            let minigroup_id = minigroup.id();

            // Set up event client mock.
            state
                .event_client_mock()
                .expect_read_room()
                .with(mockall::predicate::eq(modified_event_room_id))
                .returning(move |room_id| {
                    Ok(EventRoomResponse {
                        id: room_id,
                        time: (
                            Bound::Included(now - Duration::hours(1)),
                            Bound::Excluded(now - Duration::minutes(10)),
                        ),
                        tags: None,
                    })
                });

            state
                .event_client_mock()
                .expect_list_events()
                .withf(move |room_id: &Uuid, _kind: &str| {
                    assert_eq!(*room_id, modified_event_room_id);
                    true
                })
                .returning(move |_, kind| match kind {
                    PIN_EVENT_TYPE => Ok(vec![
                        EventBuilder::new()
                            .room_id(modified_event_room_id)
                            .set(PIN_EVENT_TYPE.to_string())
                            .data(EventData::Pin(PinEventData::new(
                                agent1.agent_id().to_owned(),
                            )))
                            .occurred_at(0)
                            .build(),
                        EventBuilder::new()
                            .room_id(modified_event_room_id)
                            .set(PIN_EVENT_TYPE.to_string())
                            .data(EventData::Pin(PinEventData::new(
                                agent2.agent_id().to_owned(),
                            )))
                            .occurred_at(1200000000000)
                            .build(),
                        EventBuilder::new()
                            .room_id(modified_event_room_id)
                            .set(PIN_EVENT_TYPE.to_string())
                            .data(EventData::Pin(PinEventData::new(
                                agent1.agent_id().to_owned(),
                            )))
                            .occurred_at(1500000000000)
                            .build(),
                    ]),
                    HOST_EVENT_TYPE => Ok(vec![EventBuilder::new()
                        .room_id(modified_event_room_id)
                        .set(HOST_EVENT_TYPE.to_string())
                        .data(EventData::Host(HostEventData::new(
                            agent1.agent_id().to_owned(),
                        )))
                        .occurred_at(0)
                        .build()]),
                    other => panic!("Event client mock got unknown kind: {}", other),
                });

            // Set up tq client mock.
            let uri1 = recording1.stream_uri().unwrap().clone();
            let uri2 = recording2.stream_uri().unwrap().clone();

            let expected_task = TqTask::TranscodeMinigroupToHls {
                streams: vec![
                    TranscodeMinigroupToHlsStream::new(recording1.rtc_id(), uri1)
                        .offset(0)
                        .segments(recording1.segments().unwrap().to_owned())
                        .pin_segments(
                            vec![
                                (Bound::Included(0), Bound::Excluded(1200000)),
                                (Bound::Included(1500000), Bound::Excluded(3000000)),
                            ]
                            .into(),
                        ),
                    TranscodeMinigroupToHlsStream::new(recording2.rtc_id(), uri2)
                        .offset(600000)
                        .segments(recording2.segments().unwrap().to_owned())
                        .pin_segments(
                            vec![(Bound::Included(600001), Bound::Excluded(900001))].into(),
                        ),
                ],
                host_stream_id: recording1.rtc_id(),
            };

            state
                .tq_client_mock()
                .expect_create_task()
                .withf(move |class: &Class, task: &TqTask| {
                    assert_eq!(class.id(), minigroup_id);
                    assert_eq!(task, &expected_task);
                    true
                })
                .returning(|_, _| Ok(()));

            // Handle event room adjustment.
            let state = Arc::new(state);

            MinigroupPostprocessingStrategy::new(state.clone(), minigroup)
                .handle_adjust(RoomAdjustResult::Success {
                    original_room_id: original_event_room_id,
                    modified_room_id: modified_event_room_id,
                    modified_segments: vec![(Bound::Included(0), Bound::Excluded(3000000))].into(),
                })
                .await
                .expect("Failed to handle event room adjustment");

            // Assert DB changes.
            let mut conn = state.get_conn().await.expect("Failed to get conn");

            let updated_minigroup = MinigroupReadQuery::by_id(minigroup_id)
                .execute(&mut conn)
                .await
                .expect("Failed to fetch minigroup")
                .expect("Minigroup not found");

            assert_eq!(
                updated_minigroup.original_event_room_id(),
                Some(original_event_room_id),
            );

            assert_eq!(
                updated_minigroup.modified_event_room_id(),
                Some(modified_event_room_id),
            );

            let recordings = RecordingListQuery::new(minigroup_id)
                .execute(&mut conn)
                .await
                .expect("Failed to fetch recordings");

            for recording in &[recording1, recording2] {
                let updated_recording = recordings
                    .iter()
                    .find(|r| r.id() == recording.id())
                    .expect("Missing recording");

                assert!(updated_recording.adjusted_at().is_some());

                assert_eq!(updated_recording.modified_segments(), recording.segments());
            }
        }

        #[async_std::test]
        async fn handle_adjust_with_pin_and_unpin() {
            let now = Utc::now();
            let agent1 = TestAgent::new("web", "user1", USR_AUDIENCE);
            let agent2 = TestAgent::new("web", "user2", USR_AUDIENCE);
            let mut state = TestState::new(TestAuthz::new()).await;
            let event_room_id = Uuid::new_v4();
            let original_event_room_id = Uuid::new_v4();
            let modified_event_room_id = Uuid::new_v4();

            // Insert a minigroup with recordings.
            let (minigroup, recording1, recording2) = {
                let mut conn = state.get_conn().await.expect("Failed to get conn");

                let time = (
                    Bound::Included(now - Duration::hours(1)),
                    Bound::Excluded(now - Duration::minutes(10)),
                );

                let minigroup_scope = format!("minigroup-{}", random_string());

                let minigroup = factory::Minigroup::new(
                    minigroup_scope,
                    USR_AUDIENCE.to_string(),
                    time.into(),
                    Uuid::new_v4(),
                    event_room_id,
                )
                .insert(&mut conn)
                .await;

                let segments1: Segments = vec![
                    (Bound::Included(0), Bound::Excluded(1500000)),
                    (Bound::Included(1800000), Bound::Excluded(3000000)),
                ]
                .into();

                let recording1 = factory::Recording::new(
                    minigroup.id(),
                    Uuid::new_v4(),
                    agent1.agent_id().to_owned(),
                )
                .segments(segments1)
                .started_at(now - Duration::hours(1))
                .stream_uri("s3://minigroup.origin.dev.example.com/rtc1.webm".to_string())
                .insert(&mut conn)
                .await;

                let recording2 = factory::Recording::new(
                    minigroup.id(),
                    Uuid::new_v4(),
                    agent2.agent_id().to_owned(),
                )
                .segments(vec![(Bound::Included(0), Bound::Excluded(2700000))].into())
                .stream_uri("s3://minigroup.origin.dev.example.com/rtc2.webm".to_string())
                .started_at(now - Duration::minutes(50))
                .insert(&mut conn)
                .await;

                (minigroup, recording1, recording2)
            };

            let minigroup_id = minigroup.id();

            // Set up event client mock.
            state
                .event_client_mock()
                .expect_read_room()
                .with(mockall::predicate::eq(modified_event_room_id))
                .returning(move |room_id| {
                    Ok(EventRoomResponse {
                        id: room_id,
                        time: (
                            Bound::Included(now - Duration::hours(1)),
                            Bound::Excluded(now - Duration::minutes(10)),
                        ),
                        tags: None,
                    })
                });

            state
                .event_client_mock()
                .expect_list_events()
                .withf(move |room_id: &Uuid, _kind: &str| {
                    assert_eq!(*room_id, modified_event_room_id);
                    true
                })
                .returning(move |_, kind| match kind {
                    PIN_EVENT_TYPE => Ok(vec![
                        EventBuilder::new()
                            .room_id(modified_event_room_id)
                            .set(PIN_EVENT_TYPE.to_string())
                            .data(EventData::Pin(PinEventData::new(
                                agent1.agent_id().to_owned(),
                            )))
                            .occurred_at(0)
                            .build(),
                        EventBuilder::new()
                            .room_id(modified_event_room_id)
                            .set(PIN_EVENT_TYPE.to_string())
                            .data(EventData::Pin(PinEventData::null()))
                            .occurred_at(1000000000000)
                            .build(),
                    ]),
                    HOST_EVENT_TYPE => Ok(vec![EventBuilder::new()
                        .room_id(modified_event_room_id)
                        .set(HOST_EVENT_TYPE.to_string())
                        .data(EventData::Host(HostEventData::new(
                            agent1.agent_id().to_owned(),
                        )))
                        .occurred_at(0)
                        .build()]),
                    other => panic!("Event client mock got unknown kind: {}", other),
                });

            // Set up tq client mock.
            let uri1 = recording1.stream_uri().unwrap().to_string();
            let uri2 = recording2.stream_uri().unwrap().to_string();

            let expected_task = TqTask::TranscodeMinigroupToHls {
                streams: vec![
                    TranscodeMinigroupToHlsStream::new(recording1.rtc_id(), uri1)
                        .offset(0)
                        .segments(recording1.segments().unwrap().to_owned())
                        .pin_segments(vec![(Bound::Included(0), Bound::Excluded(1000000))].into()),
                    TranscodeMinigroupToHlsStream::new(recording2.rtc_id(), uri2)
                        .offset(600000)
                        .segments(recording2.segments().unwrap().to_owned())
                        .pin_segments(vec![].into()),
                ],
                host_stream_id: recording1.rtc_id(),
            };

            state
                .tq_client_mock()
                .expect_create_task()
                .withf(move |class: &Class, task: &TqTask| {
                    assert_eq!(class.id(), minigroup_id);
                    assert_eq!(task, &expected_task);
                    true
                })
                .returning(|_, _| Ok(()));

            // Handle event room adjustment.
            let state = Arc::new(state);

            MinigroupPostprocessingStrategy::new(state.clone(), minigroup)
                .handle_adjust(RoomAdjustResult::Success {
                    original_room_id: original_event_room_id,
                    modified_room_id: modified_event_room_id,
                    modified_segments: vec![(Bound::Included(0), Bound::Excluded(3000000))].into(),
                })
                .await
                .expect("Failed to handle event room adjustment");

            // Assert DB changes.
            let mut conn = state.get_conn().await.expect("Failed to get conn");

            let updated_minigroup = MinigroupReadQuery::by_id(minigroup_id)
                .execute(&mut conn)
                .await
                .expect("Failed to fetch minigroup")
                .expect("Minigroup not found");

            assert_eq!(
                updated_minigroup.original_event_room_id(),
                Some(original_event_room_id),
            );

            assert_eq!(
                updated_minigroup.modified_event_room_id(),
                Some(modified_event_room_id),
            );

            let recordings = RecordingListQuery::new(minigroup_id)
                .execute(&mut conn)
                .await
                .expect("Failed to fetch recordings");

            for recording in &[recording1, recording2] {
                let updated_recording = recordings
                    .iter()
                    .find(|r| r.id() == recording.id())
                    .expect("Missing recording");

                assert!(updated_recording.adjusted_at().is_some());

                assert_eq!(updated_recording.modified_segments(), recording.segments());
            }
        }
    }

    mod handle_transcoding_completion {
        use std::ops::Bound;
        use std::sync::Arc;

        use chrono::{Duration, Utc};
        use serde_json::json;
        use uuid::Uuid;

        use crate::app::{AppContext, API_VERSION};
        use crate::db::recording::{RecordingListQuery, Segments};
        use crate::test_helpers::{prelude::*, shared_helpers::random_string};

        use super::super::super::PostprocessingStrategy;
        use super::super::*;

        #[async_std::test]
        async fn handle_transcoding_completion() {
            let now = Utc::now();
            let agent1 = TestAgent::new("web", "user1", USR_AUDIENCE);
            let agent2 = TestAgent::new("web", "user2", USR_AUDIENCE);
            let state = TestState::new(TestAuthz::new()).await;

            // Insert a minigroup with recordings.
            let (minigroup, recording1, recording2) = {
                let mut conn = state.get_conn().await.expect("Failed to get conn");

                let time = (
                    Bound::Included(now - Duration::hours(1)),
                    Bound::Excluded(now - Duration::minutes(10)),
                );

                let minigroup_scope = format!("minigroup-{}", random_string());

                let minigroup = factory::Minigroup::new(
                    minigroup_scope,
                    USR_AUDIENCE.to_string(),
                    time.into(),
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                )
                .original_event_room_id(Uuid::new_v4())
                .modified_event_room_id(Uuid::new_v4())
                .tags(json!({ "foo": "bar" }))
                .insert(&mut conn)
                .await;

                let segments1: Segments = vec![
                    (Bound::Included(0), Bound::Excluded(1500000)),
                    (Bound::Included(1800000), Bound::Excluded(3000000)),
                ]
                .into();

                let recording1 = factory::Recording::new(
                    minigroup.id(),
                    Uuid::new_v4(),
                    agent1.agent_id().to_owned(),
                )
                .stream_uri("s3://minigroup.origin.dev.example.com/rtc1.webm".to_string())
                .segments(segments1)
                .started_at(now - Duration::hours(1))
                .insert(&mut conn)
                .await;

                let recording2 = factory::Recording::new(
                    minigroup.id(),
                    Uuid::new_v4(),
                    agent2.agent_id().to_owned(),
                )
                .stream_uri("s3://minigroup.origin.dev.example.com/rtc2.webm".to_string())
                .segments(vec![(Bound::Included(0), Bound::Excluded(2700000))].into())
                .started_at(now - Duration::minutes(50))
                .insert(&mut conn)
                .await;

                (minigroup, recording1, recording2)
            };

            // Handle event room adjustment.
            let state = Arc::new(state);

            MinigroupPostprocessingStrategy::new(state.clone(), minigroup.clone())
                .handle_transcoding_completion(TranscodeSuccess::TranscodeMinigroupToHls(
                    TranscodeMinigroupToHlsSuccess {
                        recording_duration: "3000.0".to_string(),
                    },
                ))
                .await
                .expect("Failed to handle tq transcoding completion");

            // Assert DB changes.
            let mut conn = state.get_conn().await.expect("Failed to get conn");

            let updated_recordings = RecordingListQuery::new(minigroup.id())
                .execute(&mut conn)
                .await
                .expect("Failed to list recordings");

            for recording in &[recording1, recording2] {
                let updated_recording = updated_recordings
                    .iter()
                    .find(|r| r.id() == recording.id())
                    .expect("Recording not found");

                assert!(updated_recording.transcoded_at().is_some());
            }

            // Assert outgoing audience-level event.
            let messages = state.test_publisher().flush();
            let message = messages.first().expect("No event published");

            assert_eq!(
                message.topic(),
                format!(
                    "apps/{}/api/{}/audiences/{}/events",
                    state.config().id,
                    API_VERSION,
                    USR_AUDIENCE
                ),
            );

            match message.properties() {
                OutgoingEnvelopeProperties::Event(evp) => {
                    assert_eq!(evp.label(), "minigroup.ready");
                }
                props => panic!("Unexpected message properties: {:?}", props),
            }

            assert_eq!(
                message.payload::<MinigroupReady>(),
                MinigroupReady {
                    id: minigroup.id(),
                    scope: minigroup.scope().to_owned(),
                    tags: minigroup.tags().map(ToOwned::to_owned),
                    status: "success".to_string(),
                    recording_duration: 3000,
                }
            );
        }
    }
}
