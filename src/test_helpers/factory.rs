use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::postgres::PgConnection;
use svc_agent::AgentId;
use uuid::Uuid;

use crate::db::{self, recording::Segments};

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub struct P2P {
    scope: String,
    audience: String,
    conference_room_id: Uuid,
    event_room_id: Uuid,
    tags: Option<JsonValue>,
}

impl P2P {
    pub fn new(
        scope: String,
        audience: String,
        conference_room_id: Uuid,
        event_room_id: Uuid,
    ) -> Self {
        Self {
            scope,
            audience,
            conference_room_id,
            event_room_id,
            tags: None,
        }
    }

    pub fn tags(self, tags: JsonValue) -> Self {
        Self {
            tags: Some(tags),
            ..self
        }
    }

    pub async fn insert(self, conn: &mut PgConnection) -> db::class::Object {
        let mut q = db::class::P2PInsertQuery::new(
            self.scope,
            self.audience,
            self.conference_room_id,
            self.event_room_id,
        );

        if let Some(tags) = self.tags {
            q = q.tags(tags);
        }

        q.execute(conn).await.expect("Failed to insert P2P")
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub struct Minigroup {
    scope: String,
    audience: String,
    time: db::class::Time,
    conference_room_id: Uuid,
    event_room_id: Uuid,
    tags: Option<JsonValue>,
    original_event_room_id: Option<Uuid>,
    modified_event_room_id: Option<Uuid>,
}

impl Minigroup {
    pub fn new(
        scope: String,
        audience: String,
        time: db::class::Time,
        conference_room_id: Uuid,
        event_room_id: Uuid,
    ) -> Self {
        Self {
            scope,
            audience,
            time,
            conference_room_id,
            event_room_id,
            tags: None,
            original_event_room_id: None,
            modified_event_room_id: None,
        }
    }

    pub fn tags(self, tags: JsonValue) -> Self {
        Self {
            tags: Some(tags),
            ..self
        }
    }

    pub fn original_event_room_id(self, original_event_room_id: Uuid) -> Self {
        Self {
            original_event_room_id: Some(original_event_room_id),
            ..self
        }
    }

    pub fn modified_event_room_id(self, modified_event_room_id: Uuid) -> Self {
        Self {
            modified_event_room_id: Some(modified_event_room_id),
            ..self
        }
    }

    pub async fn insert(self, conn: &mut PgConnection) -> db::class::Object {
        let mut q = db::class::MinigroupInsertQuery::new(
            self.scope,
            self.audience,
            self.time,
            self.conference_room_id,
            self.event_room_id,
        );

        if let Some(tags) = self.tags {
            q = q.tags(tags);
        }

        if let Some(original_event_room_id) = self.original_event_room_id {
            q = q.original_event_room_id(original_event_room_id);
        }

        if let Some(modified_event_room_id) = self.modified_event_room_id {
            q = q.modified_event_room_id(modified_event_room_id);
        }

        q.execute(conn).await.expect("Failed to insert minigroup")
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub struct Webinar {
    scope: String,
    audience: String,
    time: db::class::Time,
    conference_room_id: Uuid,
    event_room_id: Uuid,
    tags: Option<JsonValue>,
    original_event_room_id: Option<Uuid>,
    modified_event_room_id: Option<Uuid>,
}

impl Webinar {
    pub fn new(
        scope: String,
        audience: String,
        time: db::class::Time,
        conference_room_id: Uuid,
        event_room_id: Uuid,
    ) -> Self {
        Self {
            scope,
            audience,
            time,
            conference_room_id,
            event_room_id,
            tags: None,
            original_event_room_id: None,
            modified_event_room_id: None,
        }
    }

    pub fn tags(self, tags: JsonValue) -> Self {
        Self {
            tags: Some(tags),
            ..self
        }
    }

    pub fn original_event_room_id(self, original_event_room_id: Uuid) -> Self {
        Self {
            original_event_room_id: Some(original_event_room_id),
            ..self
        }
    }

    pub fn modified_event_room_id(self, modified_event_room_id: Uuid) -> Self {
        Self {
            modified_event_room_id: Some(modified_event_room_id),
            ..self
        }
    }

    pub async fn insert(self, conn: &mut PgConnection) -> db::class::Object {
        let mut q = db::class::WebinarInsertQuery::new(
            self.scope,
            self.audience,
            self.time,
            self.conference_room_id,
            self.event_room_id,
        );

        if let Some(tags) = self.tags {
            q = q.tags(tags);
        }

        if let Some(original_event_room_id) = self.original_event_room_id {
            q = q.original_event_room_id(original_event_room_id);
        }

        if let Some(modified_event_room_id) = self.modified_event_room_id {
            q = q.modified_event_room_id(modified_event_room_id);
        }

        q.execute(conn).await.expect("Failed to insert webinar")
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub struct Chat {
    scope: String,
    audience: String,
    event_room_id: Uuid,
    tags: Option<JsonValue>,
}

impl Chat {
    pub fn new(scope: String, audience: String, event_room_id: Uuid) -> Self {
        Self {
            scope,
            audience,
            event_room_id,
            tags: None,
        }
    }

    pub fn tags(self, tags: JsonValue) -> Self {
        Self {
            tags: Some(tags),
            ..self
        }
    }

    pub async fn insert(self, conn: &mut PgConnection) -> db::chat::Object {
        let mut q = db::chat::ChatInsertQuery::new(self.scope, self.audience, self.event_room_id);

        if let Some(tags) = self.tags {
            q = q.tags(tags);
        }

        q.execute(conn).await.expect("Failed to insert chat")
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub struct Recording {
    class_id: Uuid,
    rtc_id: Uuid,
    stream_uri: Option<String>,
    segments: Option<db::recording::Segments>,
    modified_segments: Option<db::recording::Segments>,
    started_at: Option<DateTime<Utc>>,
    adjusted_at: Option<DateTime<Utc>>,
    transcoded_at: Option<DateTime<Utc>>,
    created_by: AgentId,
}

impl Recording {
    pub fn new(class_id: Uuid, rtc_id: Uuid, created_by: AgentId) -> Self {
        Self {
            class_id,
            rtc_id,
            stream_uri: None,
            segments: None,
            modified_segments: None,
            started_at: None,
            adjusted_at: None,
            transcoded_at: None,
            created_by,
        }
    }

    pub fn stream_uri(self, uri: String) -> Self {
        Self {
            stream_uri: Some(uri),
            ..self
        }
    }

    pub fn segments(self, segments: Segments) -> Self {
        Self {
            segments: Some(segments),
            ..self
        }
    }

    pub fn started_at(self, started_at: DateTime<Utc>) -> Self {
        Self {
            started_at: Some(started_at),
            ..self
        }
    }

    pub fn modified_segments(self, modified_segments: db::recording::Segments) -> Self {
        Self {
            modified_segments: Some(modified_segments),
            ..self
        }
    }

    pub fn adjusted_at(self, adjusted_at: DateTime<Utc>) -> Self {
        Self {
            adjusted_at: Some(adjusted_at),
            ..self
        }
    }

    pub fn transcoded_at(self, transcoded_at: DateTime<Utc>) -> Self {
        Self {
            transcoded_at: Some(transcoded_at),
            ..self
        }
    }

    pub async fn insert(self, conn: &mut PgConnection) -> db::recording::Object {
        let mut q =
            db::recording::RecordingInsertQuery::new(self.class_id, self.rtc_id, self.created_by);

        if let Some(modified_segments) = self.modified_segments {
            q = q.modified_segments(modified_segments);
        }

        if let Some(adjusted_at) = self.adjusted_at {
            q = q.adjusted_at(adjusted_at);
        }

        if let Some(transcoded_at) = self.transcoded_at {
            q = q.transcoded_at(transcoded_at);
        }

        if let Some(stream_uri) = self.stream_uri {
            q = q.stream_uri(stream_uri);
        }

        if let Some(segments) = self.segments {
            q = q.segments(segments);
        }

        if let Some(started_at) = self.started_at {
            q = q.started_at(started_at);
        }

        q.execute(conn).await.expect("Failed to insert recording")
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub struct Frontend {
    url: String,
}

impl Frontend {
    pub fn new(url: String) -> Self {
        Self { url }
    }

    pub async fn execute(self, conn: &mut PgConnection) -> sqlx::Result<db::frontend::Object> {
        sqlx::query_as!(
            db::frontend::Object,
            r#"
            INSERT INTO frontend (url)
            VALUES ($1)
            RETURNING id, url, created_at
            "#,
            self.url,
        )
        .fetch_one(conn)
        .await
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub struct Scope {
    scope: String,
    frontend_id: i64,
    app: String,
}

impl Scope {
    pub fn new(scope: String, frontend_id: i64, app: String) -> Self {
        Self {
            scope,
            frontend_id,
            app,
        }
    }

    pub async fn execute(self, conn: &mut PgConnection) -> sqlx::Result<db::scope::Object> {
        sqlx::query_as!(
            db::scope::Object,
            r#"
            INSERT INTO scope (scope, frontend_id, app)
            VALUES ($1, $2, $3)
            RETURNING id, scope, frontend_id, created_at, app
            "#,
            self.scope,
            self.frontend_id,
            self.app
        )
        .fetch_one(conn)
        .await
    }
}
