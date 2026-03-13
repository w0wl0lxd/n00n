use std::path::PathBuf;
use std::thread;

use crate::image;
use maki_agent::{ImageMediaType, ImageSource};

use super::App;

impl App {
    pub(super) fn start_file_image_paste(&mut self, path: PathBuf, media_type: ImageMediaType) {
        let msg = format!("Reading {}...", path.display());
        self.spawn_image_load(msg, move || image::load_file_image(&path, media_type));
    }

    pub(super) fn start_image_paste(&mut self) {
        self.spawn_image_load("Reading clipboard...".into(), image::load_clipboard_image);
    }

    fn spawn_image_load(
        &mut self,
        flash: String,
        f: impl FnOnce() -> Result<ImageSource, String> + Send + 'static,
    ) {
        let (tx, rx) = flume::bounded(1);
        thread::spawn(move || {
            let _ = tx.send(f());
        });
        self.image_paste_rx = Some(rx);
        self.status_bar.flash(flash);
    }

    pub fn poll_image_paste(&mut self) {
        let Some(ref rx) = self.image_paste_rx else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        self.image_paste_rx = None;
        match result {
            Ok(source) => {
                self.input_box.attach_image(source);
                self.status_bar.flash("Image attached".into());
            }
            Err(e) => self.status_bar.flash(format!("Image paste failed: {e}")),
        }
    }
}
