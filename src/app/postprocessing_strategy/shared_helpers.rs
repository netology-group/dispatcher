use std::ops::Bound;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};

use crate::db::recording::Segments;

use super::{MjrDumpsUploadReadyData, MjrDumpsUploadResult};

pub(super) fn extract_ready_dumps(
    rtcs: Vec<MjrDumpsUploadResult>,
) -> Result<Vec<MjrDumpsUploadReadyData>> {
    let mut ready_rtcs = Vec::with_capacity(rtcs.len());

    for rtc in rtcs {
        match rtc {
            MjrDumpsUploadResult::Ready(ready_rtc) => ready_rtcs.push(ready_rtc),
            MjrDumpsUploadResult::Missing(missing_rtc) => {
                bail!("RTC {} is missing recording", missing_rtc.id)
            }
        }
    }

    Ok(ready_rtcs)
}

pub fn parse_segments(segments: &str) -> Result<(DateTime<Utc>, Segments)> {
    let segments = segments
        .split('\n')
        .map(|line| {
            // "123456789,123.45" => (123456789, 123.45)
            match line.splitn(2, ',').collect::<Vec<&str>>().as_slice() {
                [started_at, duration] => {
                    let parsed_started_at = started_at
                        .parse::<i64>()
                        .context("Failed to parse started_at")?;

                    let parsed_duration = duration
                        .parse::<f32>()
                        .context("Failed to parse duration")?;

                    Ok((parsed_started_at, parsed_duration))
                }
                _ => bail!("Failed to split line: {}", line),
            }
        })
        .collect::<Result<Vec<_>>>()?;

    let absolute_started_at = match segments.first() {
        None => bail!("No segments parsed"),
        Some((started_at, _)) => *started_at,
    };

    // [(123456789, 123.45), (123470134, 456.78)] => [(0, 12345), (13345, 59023)]
    let relative_segments = segments
        .into_iter()
        .map(|(started_at, duration_sec)| {
            let relative_started_at = started_at - absolute_started_at;
            let duration_ms = (duration_sec * 1000.0) as i64;
            (
                Bound::Included(relative_started_at),
                Bound::Excluded(relative_started_at + duration_ms),
            )
        })
        .collect::<Vec<_>>();
    let absolute_started_at = {
        let naive_datetime = NaiveDateTime::from_timestamp(
            absolute_started_at / 1000,
            ((absolute_started_at % 1000) * 1_000_000) as u32,
        );

        DateTime::<Utc>::from_utc(naive_datetime, Utc)
    };
    Ok((absolute_started_at, relative_segments.into()))
}
