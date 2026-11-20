//! Pure conversions between program-specific data and the normalized model.
use super::model::{Cue, CueKind, Loop, Rgb, TempoSection};
use crate::core::serato_beatgrid::Beatgrid;
use crate::core::serato_markers::SeratoMarkers;

/// Moves a section that starts before 0 s forward by whole beats, advancing
/// the beat-in-bar by the same count so bar lines stay where they were.
fn shift_non_negative(pos: f64, bpm: f64, beat: u8) -> (f64, u8) {
    if pos >= 0.0 || bpm <= 0.0 {
        return (pos.max(0.0), beat);
    }
    let period = 60.0 / bpm;
    let k = (-pos / period).ceil();
    let beat = ((beat as u64 - 1 + k as u64) % 4) as u8 + 1;
    (pos + k * period, beat)
}

fn push_section(out: &mut Vec<TempoSection>, pos: f64, bpm: f64) {
    if !(bpm.is_finite() && bpm > 0.0) {
        return;
    }
    let (start, beat) = shift_non_negative(pos, bpm, 1);
    if out.last().is_some_and(|l| start <= l.start_sec) {
        return; // a shifted section may not overtake the one before it
    }
    out.push(TempoSection { start_sec: start, bpm, beat_in_bar: beat });
}

/// Serato grid → tempo sections. Every Serato marker is a downbeat, so each
/// section starts on beat 1 of a bar (the editor counts bars the same way).
pub fn grid_from_serato(g: &Beatgrid) -> Vec<TempoSection> {
    let mut out = Vec::new();
    match g {
        Beatgrid::Constant { anchor_sec, bpm } => {
            push_section(&mut out, *anchor_sec as f64, *bpm as f64);
        }
        Beatgrid::Dynamic { markers, terminal_sec, terminal_bpm } => {
            for (i, m) in markers.iter().enumerate() {
                let next = markers.get(i + 1).map(|n| n.pos_sec).unwrap_or(*terminal_sec) as f64;
                let span = next - m.pos_sec as f64;
                if m.beats_to_next == 0 || span <= 0.0 {
                    continue;
                }
                push_section(&mut out, m.pos_sec as f64, 60.0 * m.beats_to_next as f64 / span);
            }
            push_section(&mut out, *terminal_sec as f64, *terminal_bpm as f64);
        }
    }
    out
}

const CAMELOT_MUSICAL: [(&str, &str); 24] = [
    ("1A", "Abm"), ("1B", "B"), ("2A", "Ebm"), ("2B", "F#"), ("3A", "Bbm"), ("3B", "Db"),
    ("4A", "Fm"), ("4B", "Ab"), ("5A", "Cm"), ("5B", "Eb"), ("6A", "Gm"), ("6B", "Bb"),
    ("7A", "Dm"), ("7B", "F"), ("8A", "Am"), ("8B", "C"), ("9A", "Em"), ("9B", "G"),
    ("10A", "Bm"), ("10B", "D"), ("11A", "F#m"), ("11B", "A"), ("12A", "Dbm"), ("12B", "E"),
];

/// Camelot code → Rekordbox's musical key notation. None for anything else.
pub fn camelot_to_musical(c: &str) -> Option<&'static str> {
    let c = c.trim().to_ascii_uppercase();
    CAMELOT_MUSICAL.iter().find(|(k, _)| *k == c).map(|(_, m)| *m)
}

/// The palette entry closest to `c` by squared RGB distance.
pub fn nearest_colour(c: Rgb, palette: &[Rgb]) -> Rgb {
    let dist = |p: &Rgb| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(c.r, p.r) + d(c.g, p.g) + d(c.b, p.b)
    };
    palette.iter().copied().min_by_key(dist).unwrap_or(c)
}

/// Serato's per-file DJ data in model form.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileDj {
    pub cues: Vec<Cue>,
    pub loops: Vec<Loop>,
    pub colour: Option<Rgb>,
    pub grid_locked: bool,
}

fn rgb(c: [u8; 3]) -> Rgb {
    Rgb { r: c[0], g: c[1], b: c[2] }
}

/// Serato hot cues keep their slot; Serato saved loops become memory loops
/// so they never take a hot-cue slot in the target.
pub fn markers_to_model(m: &SeratoMarkers) -> FileDj {
    FileDj {
        cues: m
            .cues
            .iter()
            .map(|c| Cue {
                kind: CueKind::Hot { slot: c.slot },
                pos_sec: c.pos_ms as f64 / 1000.0,
                colour: Some(rgb(c.rgb)),
                name: c.name.clone(),
            })
            .collect(),
        loops: m
            .loops
            .iter()
            .map(|l| Loop {
                slot: None,
                start_sec: l.start_ms as f64 / 1000.0,
                end_sec: l.end_ms as f64 / 1000.0,
                colour: Some(rgb(l.rgb)),
                name: l.name.clone(),
                locked: l.locked,
            })
            .collect(),
        colour: m.colour.map(rgb),
        grid_locked: m.bpm_locked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::serato_beatgrid::Marker;
    use crate::core::serato_markers::{SeratoCue, SeratoLoop};

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn constant_grid_is_one_section_on_beat_one() {
        let s = grid_from_serato(&Beatgrid::Constant { anchor_sec: 0.0502, bpm: 131.0 });
        assert_eq!(s.len(), 1);
        assert!(close(s[0].start_sec, 0.0502));
        assert!(close(s[0].bpm, 131.0));
        assert_eq!(s[0].beat_in_bar, 1);
    }

    #[test]
    fn negative_anchor_moves_forward_by_whole_beats_keeping_the_bar() {
        // real Serato grid: marker at -0.108 s; at 120 BPM one beat is 0.5 s
        let s = grid_from_serato(&Beatgrid::Constant { anchor_sec: -0.108, bpm: 120.0 });
        assert!(close(s[0].start_sec, 0.392));
        assert_eq!(s[0].beat_in_bar, 2, "moved one beat, so the next beat of the bar");
    }

    #[test]
    fn varying_grid_gets_one_section_per_marker() {
        let s = grid_from_serato(&Beatgrid::Dynamic {
            markers: vec![
                Marker { pos_sec: 0.0, beats_to_next: 4 },
                Marker { pos_sec: 2.0, beats_to_next: 6 },
            ],
            terminal_sec: 5.0,
            terminal_bpm: 126.0,
        });
        assert_eq!(s.len(), 3);
        assert!(close(s[0].bpm, 120.0)); // 4 beats in 2 s
        assert!(close(s[1].bpm, 120.0)); // 6 beats in 3 s
        assert_eq!(s[1].beat_in_bar, 1); // 4 beats in: next bar
        assert!(close(s[2].start_sec, 5.0));
        assert!(close(s[2].bpm, 126.0));
        assert_eq!(s[2].beat_in_bar, 1, "every Serato marker is a downbeat, 10 beats in or not");
    }

    #[test]
    fn every_camelot_key_has_a_musical_name() {
        let pairs = [
            ("1A", "Abm"), ("1B", "B"), ("2A", "Ebm"), ("2B", "F#"), ("3A", "Bbm"), ("3B", "Db"),
            ("4A", "Fm"), ("4B", "Ab"), ("5A", "Cm"), ("5B", "Eb"), ("6A", "Gm"), ("6B", "Bb"),
            ("7A", "Dm"), ("7B", "F"), ("8A", "Am"), ("8B", "C"), ("9A", "Em"), ("9B", "G"),
            ("10A", "Bm"), ("10B", "D"), ("11A", "F#m"), ("11B", "A"), ("12A", "Dbm"), ("12B", "E"),
        ];
        for (c, m) in pairs {
            assert_eq!(camelot_to_musical(c), Some(m), "{c}");
        }
        assert_eq!(camelot_to_musical("Abm"), None, "not a Camelot code");
        assert_eq!(camelot_to_musical(" 8a "), Some("Am"), "trimmed, case-insensitive");
    }

    #[test]
    fn nearest_colour_picks_the_closest_palette_entry() {
        let palette = crate::core::djlib::capabilities::REKORDBOX_TRACK_COLOURS;
        assert_eq!(nearest_colour(Rgb { r: 0xcc, g: 0, b: 0 }, &palette), Rgb { r: 0xFF, g: 0, b: 0 });
        assert_eq!(nearest_colour(Rgb { r: 0x27, g: 0xaa, b: 0xe1 }, &palette), Rgb { r: 0x25, g: 0xFD, b: 0xE9 });
    }

    #[test]
    fn markers_become_hot_cues_and_memory_loops() {
        let m = SeratoMarkers {
            cues: vec![SeratoCue { slot: 2, pos_ms: 1935, rgb: [0xcc, 0, 0], name: "Drop".into() }],
            loops: vec![SeratoLoop { slot: 0, start_ms: 20, end_ms: 2340, rgb: [0x27, 0xaa, 0xe1], locked: true, name: String::new() }],
            colour: Some([0, 0xcc, 0]),
            bpm_locked: true,
        };
        let f = markers_to_model(&m);
        assert_eq!(
            f.cues,
            vec![Cue { kind: CueKind::Hot { slot: 2 }, pos_sec: 1.935, colour: Some(Rgb { r: 0xcc, g: 0, b: 0 }), name: "Drop".into() }]
        );
        assert_eq!(f.loops.len(), 1);
        assert_eq!(f.loops[0].slot, None, "Serato saved loops become memory loops");
        assert!(close(f.loops[0].start_sec, 0.02) && close(f.loops[0].end_sec, 2.34));
        assert!(f.loops[0].locked);
        assert_eq!(f.colour, Some(Rgb { r: 0, g: 0xcc, b: 0 }));
        assert!(f.grid_locked);
    }
}
