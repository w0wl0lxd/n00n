use std::path::PathBuf;
use std::thread;

use crate::image;
use n00n_agent::{ImageMediaType, ImageSource};

use super::App;

pub(super) const IMAGE_LOADING_MSG: &str = "Wait for image loading to finish before sending";
pub(super) const IMAGE_LOAD_DISCONNECTED_MSG: &str =
    "Image loader disconnected before returning a result";
const IMAGE_NOT_SUPPORTED_MSG: &str = "Model does not support image input";
pub(super) const IMAGE_STALE_MSG: &str =
    "Image finished loading after the message left; not attached";

type ImageLoadResult = Result<ImageSource, String>;

/// A running image load, tagged with the composer generation it belongs to.
#[derive(Debug)]
pub(crate) struct ImageLoad {
    rx: flume::Receiver<ImageLoadResult>,
    generation: u64,
}

impl App {
    pub(super) fn start_file_image_paste(&mut self, path: PathBuf, media_type: ImageMediaType) {
        if !self.state.model.supports_vision() {
            self.status_bar.flash(IMAGE_NOT_SUPPORTED_MSG.into());
            return;
        }
        let msg = format!("Reading {}...", path.display());
        self.spawn_image_load(msg, move || image::load_file_image(&path, media_type));
    }

    pub(super) fn start_image_paste(&mut self) {
        if !self.state.model.supports_vision() {
            self.status_bar.flash(IMAGE_NOT_SUPPORTED_MSG.into());
            return;
        }
        self.spawn_image_load("Reading clipboard...".into(), image::load_clipboard_image);
    }

    fn spawn_image_load(
        &mut self,
        flash: String,
        f: impl FnOnce() -> ImageLoadResult + Send + 'static,
    ) {
        let (tx, rx) = flume::bounded(1);
        thread::spawn(move || {
            let _ = tx.send(f());
        });
        self.track_image_load(rx);
        self.status_bar.flash(flash);
    }

    pub(super) fn track_image_load(&mut self, rx: flume::Receiver<ImageLoadResult>) {
        self.image_paste_rx.push(ImageLoad {
            rx,
            generation: self.input_box.generation(),
        });
    }

    pub fn poll_image_paste(&mut self) {
        let mut i = 0;
        while i < self.image_paste_rx.len() {
            let result = match self.image_paste_rx[i].rx.try_recv() {
                Ok(result) => result,
                Err(flume::TryRecvError::Empty) => {
                    i += 1;
                    continue;
                }
                Err(flume::TryRecvError::Disconnected) => Err(IMAGE_LOAD_DISCONNECTED_MSG.into()),
            };
            let load = self.image_paste_rx.remove(i);
            match result {
                Ok(_) if load.generation != self.input_box.generation() => {
                    self.status_bar.flash(IMAGE_STALE_MSG.into());
                }
                Ok(source) => {
                    if self.state.model.supports_vision() {
                        self.input_box.attach_image(source);
                        self.status_bar.flash("Image attached".into());
                    } else {
                        self.status_bar.flash(IMAGE_NOT_SUPPORTED_MSG.into());
                    }
                }
                Err(e) => self.status_bar.flash(format!("Image paste failed: {e}")),
            }
        }
    }
}
