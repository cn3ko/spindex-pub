//! What each DJ program can store, and what a sync to it would lose.
use serde::Serialize;

use super::model::{CueKind, DjLibrary, Rgb};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColourSupport {
    Any,
    Palette(&'static [Rgb]),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Capabilities {
    pub name: &'static str,
    pub hot_cue_slots: u8,
    pub memory_cues: bool,
    pub saved_loops: bool,
    pub track_colours: ColourSupport,
    pub varying_grid: bool,
    pub grid_lock: bool,
}

/// Rekordbox's 8 track colours as its XML writes them (spec; confirm in acceptance).
pub const REKORDBOX_TRACK_COLOURS: [Rgb; 8] = [
    Rgb { r: 0xFF, g: 0x00, b: 0x7F }, // pink
    Rgb { r: 0xFF, g: 0x00, b: 0x00 }, // red
    Rgb { r: 0xFF, g: 0xA5, b: 0x00 }, // orange
    Rgb { r: 0xFF, g: 0xFF, b: 0x00 }, // yellow
    Rgb { r: 0x00, g: 0xFF, b: 0x00 }, // green
    Rgb { r: 0x25, g: 0xFD, b: 0xE9 }, // aqua
    Rgb { r: 0x00, g: 0x00, b: 0xFF }, // blue
    Rgb { r: 0x66, g: 0x00, b: 0x99 }, // purple
];

pub const SERATO: Capabilities = Capabilities {
    name: "Serato",
    hot_cue_slots: 8,
    memory_cues: false,
    saved_loops: true,
    track_colours: ColourSupport::Any,
    varying_grid: true,
    grid_lock: true,
};

pub const REKORDBOX_XML: Capabilities = Capabilities {
    name: "Rekordbox",
    hot_cue_slots: 8,
    memory_cues: true,
    saved_loops: true,
    track_colours: ColourSupport::Palette(&REKORDBOX_TRACK_COLOURS),
    varying_grid: true,
    grid_lock: false,
};

/// One kind of data a sync would drop or round, with how many tracks it hits.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Loss {
    pub what: String,
    pub tracks: usize,
}

fn in_palette(c: Rgb, support: ColourSupport) -> bool {
    match support {
        ColourSupport::Any => true,
        ColourSupport::Palette(p) => p.contains(&c),
    }
}

/// What writing `lib` into `target` would drop or round, per kind of data.
pub fn losses(lib: &DjLibrary, target: &Capabilities) -> Vec<Loss> {
    let count = |f: &dyn Fn(&super::model::DjTrack) -> bool| lib.tracks.iter().filter(|t| f(t)).count();
    let mut out = Vec::new();
    let mut push = |what: &str, tracks: usize| {
        if tracks > 0 {
            out.push(Loss { what: what.to_string(), tracks });
        }
    };
    if !target.grid_lock {
        push("grid lock is not carried over", count(&|t| t.grid_locked));
    }
    push(
        "track colour rounded to the nearest of 8",
        count(&|t| t.colour.is_some_and(|c| !in_palette(c, target.track_colours))),
    );
    if !target.varying_grid {
        push("varying grid flattened to one tempo", count(&|t| t.grid.len() > 1));
    }
    let slots = target.hot_cue_slots;
    push(
        &format!("hot cues beyond slot {slots} are dropped"),
        count(&|t| t.cues.iter().any(|c| matches!(c.kind, CueKind::Hot { slot } if slot >= slots))),
    );
    if !target.memory_cues {
        push(
            "memory cues go to free hot-cue slots; extras are dropped",
            count(&|t| t.cues.iter().any(|c| c.kind == CueKind::Memory)),
        );
    }
    if !target.saved_loops {
        push("saved loops are dropped", count(&|t| !t.loops.is_empty()));
    }
    out
}

/// The same comparison without a library, for the dialog shown before a sync:
/// which kinds of data the target cannot hold as the source does.
pub fn capability_gaps(source: &Capabilities, target: &Capabilities) -> Vec<String> {
    let mut out = Vec::new();
    if source.grid_lock && !target.grid_lock {
        out.push("Grid lock is not carried over.".to_string());
    }
    if source.track_colours == ColourSupport::Any && target.track_colours != ColourSupport::Any {
        out.push(format!("Track colours are rounded to {}'s 8 colours.", target.name));
    }
    if source.varying_grid && !target.varying_grid {
        out.push("Varying grids are flattened to one tempo.".to_string());
    }
    if source.memory_cues && !target.memory_cues {
        out.push("Memory cues go to free hot-cue slots; extras are dropped.".to_string());
    }
    if source.saved_loops && !target.saved_loops {
        out.push("Saved loops are dropped.".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::djlib::model::{Cue, DjTrack, Loop};

    fn track() -> DjTrack {
        DjTrack::default()
    }

    #[test]
    fn serato_to_rekordbox_loses_grid_lock_and_rounds_off_palette_colours() {
        let mut locked = track();
        locked.grid_locked = true;
        let mut off_palette = track();
        off_palette.colour = Some(Rgb { r: 0x12, g: 0x34, b: 0x56 });
        let mut on_palette = track();
        on_palette.colour = Some(Rgb { r: 0xFF, g: 0x00, b: 0x00 });
        let lib = DjLibrary { tracks: vec![locked, off_palette, on_palette], playlists: vec![] };
        let l = losses(&lib, &REKORDBOX_XML);
        assert_eq!(
            l,
            vec![
                Loss { what: "grid lock is not carried over".into(), tracks: 1 },
                Loss { what: "track colour rounded to the nearest of 8".into(), tracks: 1 },
            ]
        );
    }

    #[test]
    fn nothing_is_lost_when_the_target_supports_everything() {
        let mut t = track();
        t.grid_locked = true;
        t.cues.push(Cue { kind: CueKind::Hot { slot: 7 }, pos_sec: 1.0, colour: None, name: String::new() });
        t.loops.push(Loop { slot: None, start_sec: 1.0, end_sec: 2.0, colour: None, name: String::new(), locked: false });
        let lib = DjLibrary { tracks: vec![t], playlists: vec![] };
        assert!(losses(&lib, &SERATO).is_empty());
    }

    #[test]
    fn memory_cues_and_hot_cues_beyond_the_slots_are_reported() {
        let mut t = track();
        t.cues.push(Cue { kind: CueKind::Memory, pos_sec: 1.0, colour: None, name: String::new() });
        t.cues.push(Cue { kind: CueKind::Hot { slot: 9 }, pos_sec: 2.0, colour: None, name: String::new() });
        let lib = DjLibrary { tracks: vec![t], playlists: vec![] };
        let what: Vec<String> = losses(&lib, &SERATO).into_iter().map(|l| l.what).collect();
        assert!(what.contains(&"memory cues go to free hot-cue slots; extras are dropped".to_string()));
        assert!(what.contains(&"hot cues beyond slot 8 are dropped".to_string()));
    }

    #[test]
    fn capability_gaps_serato_to_rekordbox() {
        assert_eq!(
            capability_gaps(&SERATO, &REKORDBOX_XML),
            vec![
                "Grid lock is not carried over.".to_string(),
                "Track colours are rounded to Rekordbox's 8 colours.".to_string(),
            ]
        );
    }
}
