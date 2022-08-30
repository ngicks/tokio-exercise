use std::{num::NonZeroUsize, time::Duration};

use tokio::{
    select,
    sync::{
        mpsc::{
            error::{SendError, TryRecvError},
            Receiver, Sender,
        },
        Mutex,
    },
    time::{sleep_until, Instant},
};

/// Go-like timer.
/// It can be waited for by many waiters but only one receives value at a time.
///
/// A [MovableTimer] instance is paired with single [TimerController],
/// through which you can set time and wait for the time.
pub struct MovableTimer {
    notify_sender: Sender<Instant>,
    stop_receiver: Receiver<()>,
    time_receiver: Receiver<Instant>,
}

/// A controller for single instance of [MovableTimer] it is paired with.
pub struct TimerController {
    notify_receiver: Mutex<Receiver<Instant>>,
    stop_sender: Sender<()>,
    time_sender: Sender<Instant>,
}

impl MovableTimer {
    /// new returns a pair of MovableTimer and TimerController. All channel buffers are set to 1.
    pub fn new() -> (Self, TimerController) {
        Self::with_buffer(
            NonZeroUsize::new(1).unwrap(),
            NonZeroUsize::new(1).unwrap(),
            NonZeroUsize::new(1).unwrap(),
        )
    }
    pub fn with_buffer(
        notifier_buf: NonZeroUsize,
        stopper_buf: NonZeroUsize,
        timer_buf: NonZeroUsize,
    ) -> (Self, TimerController) {
        let (notify_sender, notify_receiver) = tokio::sync::mpsc::channel(notifier_buf.get());
        let (stop_sender, stop_receiver) = tokio::sync::mpsc::channel(stopper_buf.get());
        let (time_sender, time_receiver) = tokio::sync::mpsc::channel(timer_buf.get());

        (
            Self {
                notify_sender,
                stop_receiver,
                time_receiver,
            },
            TimerController {
                notify_receiver: Mutex::new(notify_receiver),
                stop_sender,
                time_sender,
            },
        )
    }

    /// Starts wait loop for the timer.
    /// This will not be Poll::Ready state until stop of paired controller is called.
    ///
    /// # Errors
    ///
    /// Returns Err(TimerLoopError) when controller is dropped before this returns.
    pub async fn start(&mut self) -> Result<(), TimerLoopError> {
        // started as exhausted stae. time is needed to be set explicitly.
        let mut emitted = true;
        // it's far future. it's copied from tokio's internal Instant::far_future().
        let mut next_point = Instant::now() + Duration::from_secs(86400 * 365 * 30);

        loop {
            select! {
                _ =  sleep_until(next_point), if !emitted => {
                    emitted=true;
                    match self.notify_sender.send(next_point).await {
                        Ok(_) => {},
                        Err(_) => {return Err(TimerLoopError(()));},
                    };
                },
                t = self.time_receiver.recv() => {
                    emitted=false;
                    match t {
                        Some(tt) => {next_point = tt;},
                        None => {return Err(TimerLoopError(()));}
                    };
                },
                _ = self.stop_receiver.recv() => {
                    loop {
                        match self.stop_receiver.try_recv() {
                            Ok(_) => {},
                            Err(err) => {
                                match  err {
                                    TryRecvError::Empty => return Ok(()),
                                    TryRecvError::Disconnected =>  return Err(TimerLoopError(())),
                                }
                            },
                        };
                    };
                },
            }
        }
    }
}

#[derive(Debug)]
pub struct TimerLoopError(());

impl TimerController {
    pub async fn wait(&self) -> Option<Instant> {
        self.notify_receiver.lock().await.recv().await
    }
    pub async fn set_time(&self, target: Instant) -> Result<(), SendError<Instant>> {
        self.time_sender.send(target).await
    }
    pub async fn stop(&self) -> Result<(), SendError<()>> {
        self.stop_sender.send(()).await
    }
}

#[cfg(test)]
mod test {
    use more_asserts::*;
    use std::time::Duration;
    use tokio::time::Instant;

    use super::MovableTimer;

    #[tokio::test]
    async fn timer_basic_usage() {
        let (mut timer, controller) = MovableTimer::new();
        let handle = tokio::spawn(async move {
            timer.start().await.unwrap();
        });

        let now = Instant::now();
        controller
            .set_time(now + Duration::from_secs(1))
            .await
            .unwrap();
        controller.wait().await.unwrap();
        assert_ge!(Instant::now() - now, Duration::from_secs(1));

        let now = Instant::now();
        controller
            .set_time(now + Duration::from_millis(500))
            .await
            .unwrap();
        controller.wait().await.unwrap();
        assert_ge!(Instant::now() - now, Duration::from_millis(500));

        controller.stop().await.unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn controller_is_dropped_before_loop_stop() {
        let (mut timer, controller) = MovableTimer::new();
        let handle = tokio::spawn(async move { timer.start().await });

        let now = Instant::now();
        controller
            .set_time(now + Duration::from_secs(1))
            .await
            .unwrap();

        drop(controller);
        match handle.await.unwrap() {
            Ok(_) => panic!("must not be returned normally."),
            Err(_) => {}
        };
    }

    #[tokio::test]
    async fn many_reader_gets_notified_as_order_of_calling_wait() {
        let (mut timer, controller) = MovableTimer::new();
        let handle = tokio::spawn(async move {
            timer.start().await.unwrap();
        });

        let fut1 = controller.wait();
        let fut2 = controller.wait();
        let fut3 = controller.wait();

        let now = Instant::now();
        controller
            .set_time(now + Duration::from_millis(10))
            .await
            .unwrap();

        fut1.await.unwrap();
        assert_ge!(Instant::now() - now, Duration::from_millis(10));

        controller
            .set_time(now + Duration::from_millis(10))
            .await
            .unwrap();
        fut2.await.unwrap();

        controller
            .set_time(now + Duration::from_millis(10))
            .await
            .unwrap();
        fut3.await.unwrap();

        controller.stop().await.unwrap();
        handle.await.unwrap();
    }
}
