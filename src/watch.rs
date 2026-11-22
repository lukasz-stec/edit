//! Filesystem watching (ReadDirectoryChangesW on Windows), so the sidebar tree and open
//! files follow changes made on disk by other programs.

use eframe::egui;
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

pub use notify::RecursiveMode as Mode;

pub struct Change {
    pub path: PathBuf,
    /// Created, removed or renamed (the tree's shape changed), not just written.
    pub structural: bool,
}

pub struct FsWatch {
    watcher: Option<RecommendedWatcher>,
    rx: Receiver<notify::Result<Event>>,
    watched: HashMap<PathBuf, RecursiveMode>,
}

impl FsWatch {
    /// Events arrive on a background thread, which wakes the UI with a repaint.
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        let watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
            ctx.request_repaint();
        })
        .ok();
        Self { watcher, rx, watched: HashMap::new() }
    }

    /// Watch exactly the directories in `want`.
    pub fn sync(&mut self, want: &HashMap<PathBuf, RecursiveMode>) {
        let Some(w) = self.watcher.as_mut() else { return };
        self.watched.retain(|p, m| {
            let keep = want.get(p) == Some(m);
            if !keep {
                let _ = w.unwatch(p);
            }
            keep
        });
        for (p, &m) in want {
            if !self.watched.contains_key(p) && w.watch(p, m).is_ok() {
                self.watched.insert(p.clone(), m);
            }
        }
    }

    /// Pending changes, and whether events were lost so everything should be rechecked.
    pub fn drain(&self) -> (Vec<Change>, bool) {
        let (mut out, mut rescan) = (vec![], false);
        while let Ok(res) = self.rx.try_recv() {
            match res {
                Ok(ev) => {
                    rescan |= ev.need_rescan();
                    let structural = matches!(
                        ev.kind,
                        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_)) | EventKind::Any
                    );
                    out.extend(ev.paths.into_iter().map(|path| Change { path, structural }));
                }
                Err(_) => rescan = true,
            }
        }
        (out, rescan)
    }
}
