//! The adapter interface every DJ program implements.
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::Result;

use super::capabilities::Capabilities;
use super::model::DjLibrary;

/// Error text returned by an adapter that stopped because `cancel` was set.
pub const CANCELLED: &str = "cancelled";

pub struct ReadOutcome {
    pub library: DjLibrary,
    /// File names whose DJ data (grid/cues) could not be read; the tracks are
    /// still in the library with their database tags.
    pub unreadable: Vec<String>,
    /// Playlist entries that point at no indexed track.
    pub dangling: usize,
}

pub struct WriteOutcome {
    pub written: PathBuf,
}

pub trait DjEndpoint {
    fn capabilities(&self) -> &'static Capabilities;
    /// `progress(done, total)` is called per track.
    fn read(&self, progress: &dyn Fn(usize, usize), cancel: &AtomicBool) -> Result<ReadOutcome>;
    fn write(&self, lib: &DjLibrary, cancel: &AtomicBool) -> Result<WriteOutcome>;
}
