use std::ops::Bound;

use chrono::{DateTime, Utc};
use sqlx::postgres::{types::PgRange, PgConnection};
use sqlx::Done;
use svc_agent::AgentId;
use uuid::Uuid;

use serde_derive::{Deserialize, Serialize};

////////////////////////////////////////////////////////////////////////////////

#[derive(Clone, Debug)]
pub struct Object {
    id: Uuid,
    class_id: Uuid,
    rtc_id: Uuid,
    stream_uri: Option<String>,
    segments: Option<Segments>,
    modified_segments: Option<Segments>,
    started_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    adjusted_at: Option<DateTime<Utc>>,
    transcoded_at: Option<DateTime<Utc>>,
    created_by: AgentId,
    deleted_at: Option<DateTime<Utc>>,
}

impl Object {
    #[cfg(test)]
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn stream_uri(&self) -> Option<&String> {
        self.stream_uri.as_ref()
    }

    pub fn rtc_id(&self) -> Uuid {
        self.rtc_id
    }

    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        self.started_at
    }

    pub fn segments(&self) -> Option<&Segments> {
        self.segments.as_ref()
    }

    #[cfg(test)]
    pub fn modified_segments(&self) -> Option<&Segments> {
        self.modified_segments.as_ref()
    }

    pub fn adjusted_at(&self) -> Option<DateTime<Utc>> {
        self.adjusted_at
    }

    pub fn transcoded_at(&self) -> Option<DateTime<Utc>> {
        self.transcoded_at
    }

    pub fn created_by(&self) -> &AgentId {
        &self.created_by
    }
}

////////////////////////////////////////////////////////////////////////////////

pub type BoundedOffsetTuples = Vec<(Bound<i64>, Bound<i64>)>;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, sqlx::Type)]
#[sqlx(transparent)]
#[serde(from = "BoundedOffsetTuples")]
#[serde(into = "BoundedOffsetTuples")]
pub struct Segments(Vec<PgRange<i64>>);

impl From<BoundedOffsetTuples> for Segments {
    fn from(segments: BoundedOffsetTuples) -> Self {
        Self(segments.into_iter().map(PgRange::from).collect())
    }
}

impl From<Segments> for BoundedOffsetTuples {
    fn from(segments: Segments) -> Self {
        segments.0.into_iter().map(|s| (s.start, s.end)).collect()
    }
}

impl From<Segments> for Vec<PgRange<i64>> {
    fn from(segments: Segments) -> Self {
        segments.0
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct RecordingListQuery {
    class_id: Uuid,
}

impl RecordingListQuery {
    pub fn new(class_id: Uuid) -> Self {
        Self { class_id }
    }

    pub async fn execute(self, conn: &mut PgConnection) -> sqlx::Result<Vec<Object>> {
        sqlx::query_as!(
            Object,
            r#"
            SELECT
                id,
                class_id,
                rtc_id,
                stream_uri,
                segments AS "segments!: Option<Segments>",
                modified_segments AS "modified_segments!: Option<Segments>",
                started_at,
                created_at,
                adjusted_at,
                transcoded_at,
                created_by AS "created_by: AgentId",
                deleted_at
            FROM recording
            WHERE class_id = $1 AND deleted_at IS NULL
            "#,
            self.class_id
        )
        .fetch_all(conn)
        .await
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct RecordingInsertQuery {
    class_id: Uuid,
    rtc_id: Uuid,
    segments: Option<Segments>,
    started_at: Option<DateTime<Utc>>,
    stream_uri: Option<String>,
    modified_segments: Option<Segments>,
    adjusted_at: Option<DateTime<Utc>>,
    transcoded_at: Option<DateTime<Utc>>,
    created_by: AgentId,
}

impl RecordingInsertQuery {
    pub fn new(class_id: Uuid, rtc_id: Uuid, created_by: AgentId) -> Self {
        Self {
            class_id,
            rtc_id,
            segments: None,
            started_at: None,
            stream_uri: None,
            modified_segments: None,
            adjusted_at: None,
            transcoded_at: None,
            created_by,
        }
    }

    #[cfg(test)]
    pub fn modified_segments(self, modified_segments: Segments) -> Self {
        Self {
            modified_segments: Some(modified_segments),
            ..self
        }
    }

    #[cfg(test)]
    pub fn adjusted_at(self, adjusted_at: DateTime<Utc>) -> Self {
        Self {
            adjusted_at: Some(adjusted_at),
            ..self
        }
    }

    #[cfg(test)]
    pub fn transcoded_at(self, transcoded_at: DateTime<Utc>) -> Self {
        Self {
            transcoded_at: Some(transcoded_at),
            ..self
        }
    }

    #[cfg(test)]
    pub fn stream_uri(self, uri: String) -> Self {
        Self {
            stream_uri: Some(uri),
            ..self
        }
    }

    #[cfg(test)]
    pub fn segments(self, segments: Segments) -> Self {
        Self {
            segments: Some(segments),
            ..self
        }
    }

    #[cfg(test)]
    pub fn started_at(self, started_at: DateTime<Utc>) -> Self {
        Self {
            started_at: Some(started_at),
            ..self
        }
    }

    pub async fn execute(self, conn: &mut PgConnection) -> sqlx::Result<Object> {
        sqlx::query_as!(
            Object,
            r#"
            INSERT INTO recording (
                class_id, rtc_id, stream_uri, segments, modified_segments, started_at, adjusted_at,
                transcoded_at, created_by
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING
                id,
                class_id,
                rtc_id,
                stream_uri,
                segments AS "segments!: Option<Segments>",
                started_at,
                modified_segments AS "modified_segments!: Option<Segments>",
                created_at,
                adjusted_at,
                transcoded_at,
                created_by AS "created_by: AgentId",
                deleted_at
            "#,
            self.class_id,
            self.rtc_id,
            self.stream_uri,
            self.segments as Option<Segments>,
            self.modified_segments as Option<Segments>,
            self.started_at,
            self.adjusted_at,
            self.transcoded_at,
            self.created_by as AgentId,
        )
        .fetch_one(conn)
        .await
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct AdjustWebinarUpdateQuery {
    webinar_id: Uuid,
    modified_segments: Segments,
}

impl AdjustWebinarUpdateQuery {
    pub fn new(webinar_id: Uuid, modified_segments: Segments) -> Self {
        Self {
            webinar_id,
            modified_segments,
        }
    }

    pub async fn execute(self, conn: &mut PgConnection) -> sqlx::Result<Object> {
        sqlx::query_as!(
            Object,
            r#"
            UPDATE recording
            SET modified_segments = $2,
                adjusted_at = NOW()
            WHERE class_id = $1 AND deleted_at IS NULL
            RETURNING
                id,
                class_id,
                rtc_id,
                stream_uri,
                segments AS "segments!: Option<Segments>",
                started_at,
                modified_segments AS "modified_segments!: Option<Segments>",
                created_at,
                adjusted_at,
                transcoded_at,
                created_by AS "created_by: AgentId",
                deleted_at
            "#,
            self.webinar_id,
            self.modified_segments as Segments,
        )
        .fetch_one(conn)
        .await
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct AdjustMinigroupUpdateQuery {
    minigroup_id: Uuid,
}

impl AdjustMinigroupUpdateQuery {
    pub fn new(minigroup_id: Uuid) -> Self {
        Self { minigroup_id }
    }

    pub async fn execute(self, conn: &mut PgConnection) -> sqlx::Result<Vec<Object>> {
        sqlx::query_as!(
            Object,
            r#"
            UPDATE recording
            SET modified_segments = segments,
                adjusted_at = NOW()
            WHERE class_id = $1
            RETURNING
                id,
                class_id,
                rtc_id,
                stream_uri,
                segments AS "segments!: Option<Segments>",
                started_at,
                modified_segments AS "modified_segments!: Option<Segments>",
                created_at,
                adjusted_at,
                transcoded_at,
                created_by AS "created_by: AgentId",
                deleted_at
            "#,
            self.minigroup_id,
        )
        .fetch_all(conn)
        .await
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct TranscodingUpdateQuery {
    class_id: Uuid,
}

impl TranscodingUpdateQuery {
    pub fn new(class_id: Uuid) -> Self {
        Self { class_id }
    }

    pub async fn execute(self, conn: &mut PgConnection) -> sqlx::Result<Object> {
        sqlx::query_as!(
            Object,
            r#"
            UPDATE recording
            SET transcoded_at = NOW()
            WHERE class_id = $1 AND deleted_at IS NULL
            RETURNING
                id,
                class_id,
                rtc_id,
                stream_uri,
                segments AS "segments!: Option<Segments>",
                started_at,
                modified_segments AS "modified_segments!: Option<Segments>",
                created_at,
                adjusted_at,
                transcoded_at,
                created_by AS "created_by: AgentId",
                deleted_at
            "#,
            self.class_id,
        )
        .fetch_one(conn)
        .await
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct RecordingConvertInsertQuery {
    class_id: Uuid,
    rtc_id: Uuid,
    segments: Segments,
    modified_segments: Segments,
    stream_uri: String,
    created_by: AgentId,
}

impl RecordingConvertInsertQuery {
    pub fn new(
        class_id: Uuid,
        rtc_id: Uuid,
        segments: Segments,
        modified_segments: Segments,
        stream_uri: String,
        created_by: AgentId,
    ) -> Self {
        Self {
            class_id,
            rtc_id,
            segments,
            modified_segments,
            stream_uri,
            created_by,
        }
    }

    pub async fn execute(self, conn: &mut PgConnection) -> sqlx::Result<Object> {
        sqlx::query_as!(
            Object,
            r#"
            INSERT INTO recording (class_id, rtc_id, segments, modified_segments, stream_uri, started_at, adjusted_at, transcoded_at, created_by)
            VALUES ($1, $2, $3, $4, $5, NOW(), NOW(), NOW(), $6)
            RETURNING
                id,
                class_id,
                rtc_id,
                stream_uri,
                segments AS "segments!: Option<Segments>",
                started_at,
                modified_segments AS "modified_segments!: Option<Segments>",
                created_at,
                adjusted_at,
                transcoded_at,
                created_by AS "created_by: AgentId",
                deleted_at
            "#,
            self.class_id,
            self.rtc_id,
            self.segments as Segments,
            self.modified_segments as Segments,
            self.stream_uri,
            self.created_by as AgentId

        )
        .fetch_one(conn)
        .await
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct StreamUploadUpdateQuery {
    class_id: Uuid,
    rtc_id: Uuid,
    segments: Segments,
    stream_uri: String,
    started_at: DateTime<Utc>,
}

impl StreamUploadUpdateQuery {
    pub fn new(
        class_id: Uuid,
        rtc_id: Uuid,
        segments: Segments,
        stream_uri: String,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self {
            class_id,
            rtc_id,
            segments,
            stream_uri,
            started_at,
        }
    }

    pub async fn execute(self, conn: &mut PgConnection) -> sqlx::Result<Object> {
        sqlx::query_as!(
            Object,
            r#"
            UPDATE recording
            SET segments = $3,
                stream_uri = $4,
                started_at = $5
            WHERE class_id = $1  AND rtc_id = $2 AND deleted_at IS NULL
            RETURNING
                id,
                class_id,
                rtc_id,
                stream_uri,
                segments AS "segments!: Option<Segments>",
                started_at,
                modified_segments AS "modified_segments!: Option<Segments>",
                created_at,
                adjusted_at,
                transcoded_at,
                created_by AS "created_by: AgentId",
                deleted_at
            "#,
            self.class_id,
            self.rtc_id,
            self.segments as Segments,
            self.stream_uri,
            self.started_at,
        )
        .fetch_one(conn)
        .await
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct DeleteQuery {
    class_id: Uuid,
}

impl DeleteQuery {
    pub fn new(class_id: Uuid) -> Self {
        Self { class_id }
    }

    pub(crate) async fn execute(self, conn: &mut PgConnection) -> sqlx::Result<usize> {
        sqlx::query_as!(
            Object,
            r#"
            UPDATE recording
            SET deleted_at = NOW()
            WHERE class_id = $1 AND deleted_at IS NULL
            "#,
            self.class_id,
        )
        .execute(conn)
        .await
        .map(|r| r.rows_affected() as usize)
    }
}

////////////////////////////////////////////////////////////////////////////////

pub(crate) mod serde {
    pub(crate) mod segments {
        use super::super::{BoundedOffsetTuples, Segments};
        use crate::serde::milliseconds_bound_tuples;
        use serde::{de, ser};

        pub(crate) fn serialize<S>(value: &Segments, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: ser::Serializer,
        {
            let bounded_offset_tuples: BoundedOffsetTuples = value.to_owned().into();
            milliseconds_bound_tuples::serialize(&bounded_offset_tuples, serializer)
        }

        pub(crate) fn deserialize<'de, D>(d: D) -> Result<Segments, D::Error>
        where
            D: de::Deserializer<'de>,
        {
            milliseconds_bound_tuples::deserialize(d).map(Segments::from)
        }
    }

    pub(crate) fn segments_option<S>(
        opt: &Option<super::Segments>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        match opt {
            Some(value) => segments::serialize(value, serializer),
            None => serializer.serialize_none(),
        }
    }
}
