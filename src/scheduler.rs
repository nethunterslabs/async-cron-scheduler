// Copyright 2021 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

use crate::{Job, JobId, TimeZoneExt};
use chrono::DateTime;
use futures::future::Either;
use slotmap::{DefaultKey, SecondaryMap, SlotMap};
use std::{future::Future, time::Duration};
use tachyonix::Sender;

/// A scheduled command associated with a job.
pub type Command = Box<dyn Fn(JobId) + Send + Sync>;

/// Messages going into the scheduler service.
enum SchedMessage<Tz: TimeZoneExt> {
    Insert(JobId, Box<Job<Tz>>, Command),
    Remove(JobId),
}

/// The interface for interacting with the scheduler.
///
/// When launching a scheduler, the scheduler and its service are created
/// simultaneously with a channel connecting them. Job insert and remove
/// messages are sent to the service for automatic management. When the
/// scheduler is dropped, so too will the service its attached to exit.
///
/// ```
/// use smol::Timer;
/// use chrono::offset::Local;
///
/// let (mut scheduler, service) = Scheduler::<Local>::launch(Timer::after);
///
/// // Creates a job which executes every 3 seconds.
/// let job = Job::cron("1-59/3 * * * * *").unwrap();
/// let fizz_id = scheduler.insert(job, |id| println!("Fizz")).await;
///
/// // Creates a job which executes every 5 seconds.
/// let job = Job::cron("1-59/5 * * * * *").unwrap();
/// let buzz_id = scheduler.insert(job, |id| println!("Buzz")).await;
///
/// service.await;
/// ```
pub struct Scheduler<Tz: TimeZoneExt> {
    jobs: SlotMap<DefaultKey, ()>,
    sender: Sender<SchedMessage<Tz>>,
}

impl<Tz: TimeZoneExt + 'static> Scheduler<Tz>
where
    Tz::Offset: Send + Sync,
{
    /// Insert a job into the scheduler with the command to call when scheduled.
    ///
    /// ```
    /// // Creates a job which executes every 3 seconds.
    /// let job = Job::cron("1-59/3 * * * * *").unwrap();
    /// let fizz_id = scheduler.insert(job, |id| println!("Fizz")).await;
    /// ```
    pub async fn insert(
        &mut self,
        job: Job<Tz>,
        command: impl Fn(JobId) + Send + Sync + 'static,
    ) -> JobId {
        let id = JobId(self.jobs.insert(()));
        let _result = self
            .sender
            .send(SchedMessage::Insert(id, Box::new(job), Box::new(command)))
            .await;
        id
    }

    /// Remove a scheduled job from the scheduler.
    ///
    /// ```
    /// scheduler.remove(fizz_id).await;
    /// ```
    pub async fn remove(&mut self, job: JobId) {
        if self.jobs.remove(job.0).is_some() {
            let _res = self.sender.send(SchedMessage::Remove(job)).await;
        }
    }

    /// Initializes the scheduler and its connected service.
    ///
    /// The API is designed to not rely on any async runtimes. This is achieved by
    /// returning a future to allow the caller to decide how it should be executed,
    /// and taking a function for handling sleeps. You can choose to spawn the
    /// returned future, or avoid spawning altgether and await it directly from the
    /// same thread.
    ///
    /// ## Smol runtime
    ///
    /// ```
    /// let (mut scheduler, sched_service) = Scheduler::<Local>::launch(smol::Timer::after);
    /// smol::spawn(sched_service).detach();
    /// ```
    ///
    /// ## Tokio runtime
    ///
    /// ```
    /// let (mut scheduler, sched_service) = Scheduler::<Local>::launch(tokio::time::sleep);
    /// tokio::spawn(sched_service);
    /// ```
    pub fn launch<F, T, X>(timer: T) -> (Self, impl Future<Output = ()> + Send + Sync + 'static)
    where
        F: Future<Output = X> + Send + Sync,
        T: Fn(Duration) -> F + Send + Sync + 'static,
    {
        let (sender, mut receiver) = tachyonix::channel(16);

        let task = async move {
            let mut state = SchedulerModel {
                tasks: SecondaryMap::new(),
                next: None,
            };

            loop {
                match state.next.take() {
                    Some((key, duration)) => {
                        let message = receiver.recv();
                        let wait = timer(duration);

                        futures::pin_mut!(message);
                        futures::pin_mut!(wait);

                        match futures::future::select(message, wait).await {
                            Either::Left((message, _)) => match message {
                                Ok(message) => state.update(message),
                                Err(_) => break,
                            },

                            Either::Right((_, _)) => {
                                state.call(key);
                                state.next();
                            }
                        }
                    }

                    None => match receiver.recv().await {
                        Ok(message) => state.update(message),
                        Err(_) => break,
                    },
                };
            }
        };

        (
            Self {
                sender,
                jobs: SlotMap::new(),
            },
            task,
        )
    }
}
struct SchedulerModel<Tz: TimeZoneExt> {
    tasks: SecondaryMap<DefaultKey, (Job<Tz>, Command)>,
    next: Option<(DefaultKey, Duration)>,
}

impl<Tz: TimeZoneExt> SchedulerModel<Tz> {
    pub fn call(&mut self, key: DefaultKey) {
        if let Some((job, func)) = self.tasks.get_mut(key) {
            func(JobId(key));

            if let Some(next) = job.iterator.next() {
                job.next = next;

                return;
            }

            self.tasks.remove(key);
        }
    }

    pub fn next(&mut self) {
        loop {
            let mut next: Option<(DefaultKey, DateTime<Tz>)> = None;

            for (id, (job, _)) in &self.tasks {
                if let Some((_, date_time)) = next.as_ref() {
                    if date_time.timestamp() > job.next.timestamp() {
                        next = Some((id, job.next.clone()));
                    }

                    continue;
                }

                next = Some((id, job.next.clone()));
            }

            if let Some((id, date)) = next {
                // Wait the full remaining time, sub-second part included, so a job never
                // runs before its instant. Only a zero or negative wait runs at once.
                if let Ok(duration) = date.signed_duration_since(Tz::now()).to_std() {
                    if !duration.is_zero() {
                        #[cfg(feature = "logging")]
                        tracing::info!("next job in {:?}", duration);

                        self.next = Some((id, duration));
                        return;
                    }
                }

                self.call(id);
                continue;
            }

            return;
        }
    }

    pub fn update(&mut self, message: SchedMessage<Tz>) {
        match message {
            SchedMessage::Insert(id, job, func) => {
                self.tasks.insert(id.0, (*job, func));
                self.next();
            }

            SchedMessage::Remove(id) => {
                self.tasks.remove(id.0);
                self.next();
            }
        }
    }
}

#[cfg(all(test, feature = "croner"))]
mod tests {
    use super::Scheduler;
    use crate::Job;
    use chrono::{DateTime, Utc};
    use croner::parser::{CronParser, Seconds};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A job must not run before its scheduled instant. `next()` truncates the wait to
    /// whole seconds (`num_seconds()`), so a job inserted mid-second runs up to a second
    /// early, every time: a per-second job fires at x.5 instead of x+1.0.
    #[test]
    fn fires_at_or_after_the_scheduled_instant() {
        smol::block_on(async {
            // Start half way through a second, where truncation loses the most.
            let subsec = i64::from(Utc::now().timestamp_subsec_nanos());
            let to_mid = (1_500_000_000 - subsec).rem_euclid(1_000_000_000);
            smol::Timer::after(Duration::from_nanos(to_mid as u64)).await;

            let (mut scheduler, service) = Scheduler::<Utc>::launch(smol::Timer::after);
            let fires: Arc<Mutex<Vec<DateTime<Utc>>>> = Arc::default();
            let cron = CronParser::builder()
                .seconds(Seconds::Required)
                .build()
                .parse("* * * * * *")
                .unwrap();
            let recorded = fires.clone();
            scheduler
                .insert(Job::cron_schedule(cron), move |_| {
                    recorded.lock().unwrap().push(Utc::now());
                })
                .await;

            let stop = async move {
                smol::Timer::after(Duration::from_millis(3_200)).await;
                drop(scheduler);
            };
            futures::future::join(service, stop).await;

            let fires = fires.lock().unwrap().clone();
            // Every fire is for a whole second and must land just after it, never 0.5 s
            // before it. The first is skipped in case the job was created on a boundary.
            let early: Vec<_> = fires
                .iter()
                .skip(1)
                .filter(|t| t.timestamp_subsec_millis() > 200)
                .collect();
            assert!(
                early.is_empty(),
                "fires before their instant: {early:?} of {fires:?}"
            );
        });
    }
}
