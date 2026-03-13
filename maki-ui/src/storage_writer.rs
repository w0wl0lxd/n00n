use std::sync::{Arc, Mutex};
use std::time::Duration;

use maki_storage::DataDir;
use tracing::warn;

use crate::AppSession;

pub struct StorageWriter {
    latest: Arc<Mutex<Option<Box<AppSession>>>>,
    notify: flume::Sender<()>,
    done_rx: flume::Receiver<()>,
}

impl StorageWriter {
    pub fn new(dir: DataDir) -> Self {
        let latest: Arc<Mutex<Option<Box<AppSession>>>> = Arc::new(Mutex::new(None));
        let writer_latest = Arc::clone(&latest);
        let (notify, notify_rx) = flume::bounded::<()>(1);
        let (done_tx, done_rx) = flume::bounded::<()>(1);

        std::thread::Builder::new()
            .name("storage-writer".into())
            .spawn(move || {
                while notify_rx.recv().is_ok() {
                    let session = writer_latest.lock().unwrap().take();
                    if let Some(mut session) = session
                        && let Err(e) = session.save(&dir)
                    {
                        warn!(error = %e, "background save failed");
                    }
                }
                let _ = done_tx.send(());
            })
            .expect("failed to spawn storage writer thread");

        Self {
            latest,
            notify,
            done_rx,
        }
    }

    pub fn send(&self, session: Box<AppSession>) {
        *self.latest.lock().unwrap() = Some(session);
        let _ = self.notify.try_send(());
    }

    pub fn shutdown(self, timeout: Duration) {
        drop(self.notify);
        if self.done_rx.recv_timeout(timeout).is_err() {
            warn!("storage writer did not drain within {timeout:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;

    #[test]
    fn shutdown_drains_pending_session() {
        let dir = DataDir::from_path(env::temp_dir().join("maki-test-sw"));
        let writer = StorageWriter::new(dir);
        writer.send(Box::new(AppSession::new("test-model", "/tmp")));
        writer.shutdown(Duration::from_secs(2));
    }
}
