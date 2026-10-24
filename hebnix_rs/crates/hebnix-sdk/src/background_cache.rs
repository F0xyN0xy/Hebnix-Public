//! Publish immutable snapshots without making readers wait for discovery or I/O.
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

pub(crate) struct BackgroundCache<T> {
    value: Arc<Mutex<Arc<T>>>,
    refresh: mpsc::SyncSender<()>,
}

impl<T: Send + Sync + 'static> BackgroundCache<T> {
    pub(crate) fn new(
        name: &str,
        initial: T,
        interval: Duration,
        mut load: impl FnMut(bool) -> Option<T> + Send + 'static,
    ) -> Self {
        let value = Arc::new(Mutex::new(Arc::new(initial)));
        let (refresh, requests) = mpsc::sync_channel(1);
        let published = Arc::clone(&value);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let mut force = true;
                loop {
                    // Discovery, decoding and filesystem work must stay outside the lock.
                    if let Some(next) = load(force) {
                        let previous = std::mem::replace(
                            &mut *published.lock().unwrap_or_else(|e| e.into_inner()),
                            Arc::new(next),
                        );
                        drop(previous);
                    }
                    force = match requests.recv_timeout(interval) {
                        Ok(()) => true,
                        Err(mpsc::RecvTimeoutError::Timeout) => false,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                }
            })
            .expect("failed to spawn background cache worker");
        Self { value, refresh }
    }

    pub(crate) fn snapshot(&self) -> Arc<T> {
        Arc::clone(&self.value.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub(crate) fn refresh(&self) {
        // Coalesce repeated invalidations; a UI reader must never wait for a worker.
        let _ = self.refresh.try_send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_refresh_keeps_readers_responsive_and_preserves_the_last_snapshot() {
        let (started, starts) = mpsc::channel();
        let (finish, finishes) = mpsc::channel();
        let cache = Arc::new(BackgroundCache::new(
            "cache-test",
            7,
            Duration::from_secs(60),
            move |force| {
                started.send(force).unwrap();
                finishes.recv().unwrap()
            },
        ));
        assert!(starts.recv_timeout(Duration::from_secs(5)).unwrap());
        let (read, reads) = mpsc::channel();
        let reader = Arc::clone(&cache);
        std::thread::spawn(move || {
            read.send(reader.snapshot()).unwrap();
        });
        // The loader remains blocked until after this read has completed.
        assert_eq!(*reads.recv_timeout(Duration::from_secs(5)).unwrap(), 7);
        finish.send(Some(11)).unwrap();
        cache.refresh();
        assert!(starts.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(*cache.snapshot(), 11);
        finish.send(None).unwrap();
        cache.refresh();
        assert!(starts.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(*cache.snapshot(), 11);
        drop(cache);
        finish.send(None).unwrap();
    }
}
