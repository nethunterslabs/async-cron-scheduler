// Copyright 2021 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

use crate::TimeZoneExt;
use chrono::DateTime;
use std::str::FromStr;

/// The ID of a scheduled job.
#[allow(clippy::module_name_repetitions)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct JobId(pub slotmap::DefaultKey);

/// Contains scheduling information for a job at a given timezone.
pub struct Job<Tz: TimeZoneExt> {
    #[cfg(feature = "cron")]
    pub(crate) iterator: cron::OwnedScheduleIterator<Tz>,
    #[cfg(feature = "croner")]
    pub(crate) iterator: croner::CronIterator<Tz>,
    pub(crate) next: DateTime<Tz>,
}

impl<Tz: TimeZoneExt> Job<Tz> {
    /// Creates a job from a cron expression string.
    ///
    /// # Errors
    ///
    /// Errors if the cron expression is invalid.
    #[cfg(feature = "cron")]
    pub fn cron(expression: &str) -> Result<Self, cron::error::Error> {
        cron::Schedule::from_str(expression).map(Job::cron_schedule)
    }

    /// Creates a job from a cron expression string.
    ///
    /// # Errors
    ///
    /// Errors if the cron expression is invalid.
    #[cfg(feature = "croner")]
    pub fn cron(expression: &str) -> Result<Self, croner::errors::CronError> {
        croner::Cron::from_str(expression).map(Job::cron_schedule)
    }

    /// Creates a job from a pre-generated cron schedule.
    ///
    /// # Panics
    ///
    /// Panics at the end of time.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    #[cfg(feature = "cron")]
    pub fn cron_schedule(schedule: cron::Schedule) -> Self {
        let mut iterator = schedule.upcoming_owned(Tz::timescale());
        let next = iterator.next().unwrap();
        Job { iterator, next }
    }

    /// Creates a job from a pre-generated cron schedule.
    ///
    /// # Panics
    ///
    /// Panics at the end of time.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    #[cfg(feature = "croner")]
    pub fn cron_schedule(schedule: croner::Cron) -> Self {
        let mut iterator = schedule.iter_from(Tz::now(), croner::Direction::Forward);
        let next = iterator.next().unwrap();
        Job { iterator, next }
    }
}
