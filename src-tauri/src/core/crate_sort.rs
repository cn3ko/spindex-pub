//! One-shot re-ordering of a whole crate (the "Sort ▾" helper): by key, by
//! BPM, shuffled, or as a harmonic mix that walks the Camelot wheel. Pure —
//! the caller reads the tracks in their current order and writes the result
//! through the crate's normal order path (`crate_order::apply_order` or the
//! Serato `.crate` writer).

use crate::core::db::key_spellings;

/// What the user picked in the Sort menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    /// 1A, 1B, 2A, … 12B
    KeyAsc,
    /// 12B, 12A, 11B, … 1A
    KeyDesc,
    /// harmonic mix from a random first track
    Harmonic,
    /// harmonic mix that keeps the current first track as the opener
    HarmonicFromFirst,
    /// slowest first
    Bpm,
    /// plain shuffle
    Random,
}

impl SortMode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "key_asc" => Self::KeyAsc,
            "key_desc" => Self::KeyDesc,
            "harmonic" => Self::Harmonic,
            "harmonic_first" => Self::HarmonicFromFirst,
            "bpm" => Self::Bpm,
            "random" => Self::Random,
            _ => return None,
        })
    }
}

/// A Camelot key: wheel number 1..=12 and minor (A) or major (B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub num: u8,
    pub minor: bool,
}

/// One crate track as the sort sees it.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: i64,
    pub key: Option<Key>,
    pub bpm: Option<f64>,
}

/// A stored key value ("8A", "08a", "Am", "A minor", "G#m", …) as a Camelot
/// key. Uses the same spelling table as the key filter, so anything the
/// filter matches, the sort understands.
pub fn parse_key(raw: &str) -> Option<Key> {
    let norm: String = raw.trim().to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if norm.is_empty() {
        return None;
    }
    for num in 1..=12u8 {
        for (letter, minor) in [("A", true), ("B", false)] {
            if key_spellings(&format!("{num}{letter}")).contains(&norm) {
                return Some(Key { num, minor });
            }
        }
    }
    None
}

/// Steps between two wheel numbers, the short way round (0..=6).
fn wheel_steps(a: Key, b: Key) -> u8 {
    let d = a.num.abs_diff(b.num);
    d.min(12 - d)
}

/// The mixing rule: the same key, its relative major/minor (7A ↔ 7B), or up
/// to two steps either way on the same ring (7A → 5A, 6A, 8A, 9A).
pub fn compatible(a: Key, b: Key) -> bool {
    if a.minor == b.minor {
        wheel_steps(a, b) <= 2
    } else {
        wheel_steps(a, b) == 0
    }
}

/// How far apart two keys are, for picking the least bad jump when nothing
/// compatible is left.
fn distance(a: Key, b: Key) -> u32 {
    wheel_steps(a, b) as u32 + u32::from(a.minor != b.minor)
}

/// Small xorshift64* generator: a shuffle needs no crypto and no new crate.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    pub fn from_clock() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Rng::new(nanos ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// 0..n (n > 0)
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i + 1);
            v.swap(i, j);
        }
    }
}

/// The crate's new order. `items` is the crate in its current order; the
/// result holds every id exactly once.
pub fn sort_order(items: &[Item], mode: SortMode, rng: &mut Rng) -> Vec<i64> {
    match mode {
        SortMode::KeyAsc | SortMode::KeyDesc => by_key(items, mode == SortMode::KeyDesc),
        SortMode::Bpm => by_bpm(items),
        SortMode::Random => {
            let mut ids: Vec<i64> = items.iter().map(|t| t.id).collect();
            rng.shuffle(&mut ids);
            ids
        }
        SortMode::Harmonic => harmonic(items, false, rng),
        SortMode::HarmonicFromFirst => harmonic(items, true, rng),
    }
}

/// Wheel order; within one key the slower track first. Tracks without a key
/// go last, in their current order.
fn by_key(items: &[Item], descending: bool) -> Vec<i64> {
    let rank = |k: Key| k.num as u32 * 2 + u32::from(!k.minor);
    let mut keyed: Vec<(&Item, Key)> =
        items.iter().filter_map(|t| t.key.map(|k| (t, k))).collect();
    keyed.sort_by(|(a, ka), (b, kb)| {
        let (ra, rb) = (rank(*ka), rank(*kb));
        let by = if descending { rb.cmp(&ra) } else { ra.cmp(&rb) };
        by.then_with(|| bpm_cmp(a.bpm, b.bpm))
    });
    keyed
        .iter()
        .map(|(t, _)| t.id)
        .chain(items.iter().filter(|t| t.key.is_none()).map(|t| t.id))
        .collect()
}

/// Missing BPM sorts after every real one.
fn bpm_cmp(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    let v = |x: Option<f64>| x.filter(|b| *b > 0.0).unwrap_or(f64::INFINITY);
    v(a).total_cmp(&v(b))
}

/// Slowest first; tracks without a BPM last (stable, so ties keep their order).
fn by_bpm(items: &[Item]) -> Vec<i64> {
    let mut v: Vec<&Item> = items.iter().collect();
    v.sort_by(|a, b| bpm_cmp(a.bpm, b.bpm));
    v.iter().map(|t| t.id).collect()
}

/// A walk around the wheel where every step follows the mixing rule
/// (`compatible`) whenever the remaining tracks allow it. Each step picks a
/// random compatible track; several random walks are tried and the one with
/// the fewest clashes wins, so a crate that CAN be mixed end to end usually
/// is. Tracks without a key go last, in their current order.
fn harmonic(items: &[Item], keep_first: bool, rng: &mut Rng) -> Vec<i64> {
    let keyed: Vec<(i64, Key)> = items.iter().filter_map(|t| t.key.map(|k| (t.id, k))).collect();
    let unkeyed = items.iter().filter(|t| t.key.is_none()).map(|t| t.id);

    // the opener the user put first; without a key it simply stays on top
    let first = if keep_first { items.first() } else { None };
    let mut out: Vec<i64> = Vec::with_capacity(items.len());
    let start = match first {
        Some(t) => match t.key {
            Some(_) => keyed.iter().position(|(id, _)| *id == t.id),
            None => {
                out.push(t.id);
                None
            }
        },
        None => None,
    };

    if !keyed.is_empty() {
        // n² work per walk; keep the total bounded for big crates
        let n = keyed.len();
        let tries = (4_000_000 / (n * n).max(1)).clamp(1, 64);
        let mut best: Option<(u32, u32, Vec<usize>)> = None;
        for _ in 0..tries {
            let s = start.unwrap_or_else(|| rng.below(n));
            let (clashes, jump, walk) = walk_once(&keyed, s, rng);
            if best.as_ref().is_none_or(|(c, j, _)| (clashes, jump) < (*c, *j)) {
                best = Some((clashes, jump, walk));
            }
            if clashes == 0 && start.is_some() {
                break;
            }
        }
        if let Some((_, _, walk)) = best {
            out.extend(walk.into_iter().map(|i| keyed[i].0));
        }
    }

    let placed: std::collections::HashSet<i64> = out.iter().copied().collect();
    out.extend(unkeyed.filter(|id| !placed.contains(id)));
    out
}

/// One random walk from `start`. Returns (clashes, total distance of the
/// clashing jumps, indexes into `keyed` in play order).
fn walk_once(keyed: &[(i64, Key)], start: usize, rng: &mut Rng) -> (u32, u32, Vec<usize>) {
    let mut left: Vec<usize> = (0..keyed.len()).filter(|&i| i != start).collect();
    let mut walk = vec![start];
    let (mut clashes, mut jump) = (0u32, 0u32);
    let mut cur = keyed[start].1;
    while !left.is_empty() {
        let fits: Vec<usize> =
            (0..left.len()).filter(|&j| compatible(cur, keyed[left[j]].1)).collect();
        let pick = if fits.is_empty() {
            // nothing mixes: take the nearest key (random among equals)
            let best = left.iter().map(|&i| distance(cur, keyed[i].1)).min().unwrap_or(0);
            let near: Vec<usize> =
                (0..left.len()).filter(|&j| distance(cur, keyed[left[j]].1) == best).collect();
            clashes += 1;
            jump += best;
            near[rng.below(near.len())]
        } else {
            fits[rng.below(fits.len())]
        };
        let i = left.swap_remove(pick);
        cur = keyed[i].1;
        walk.push(i);
    }
    (clashes, jump, walk)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Key {
        parse_key(s).unwrap()
    }

    fn item(id: i64, key: Option<&str>, bpm: Option<f64>) -> Item {
        Item { id, key: key.and_then(parse_key), bpm }
    }

    fn is_permutation(items: &[Item], out: &[i64]) -> bool {
        let mut a: Vec<i64> = items.iter().map(|t| t.id).collect();
        let mut b = out.to_vec();
        a.sort();
        b.sort();
        a == b
    }

    #[test]
    fn parses_codes_and_musical_names() {
        assert_eq!(parse_key("8A"), Some(Key { num: 8, minor: true }));
        assert_eq!(parse_key("08a"), Some(Key { num: 8, minor: true }));
        assert_eq!(parse_key("Am"), Some(Key { num: 8, minor: true }));
        assert_eq!(parse_key("A minor"), Some(Key { num: 8, minor: true }));
        assert_eq!(parse_key("C"), Some(Key { num: 8, minor: false }));
        assert_eq!(parse_key("G#m"), Some(Key { num: 1, minor: true }));
        assert_eq!(parse_key(""), None);
        assert_eq!(parse_key("13A"), None);
    }

    #[test]
    fn mixing_rule_is_two_steps_on_a_ring_or_the_relative_key() {
        let from = k("7A");
        for ok in ["7A", "7B", "8A", "9A", "6A", "5A"] {
            assert!(compatible(from, k(ok)), "7A → {ok} should mix");
        }
        for bad in ["10A", "4A", "8B", "6B", "1A"] {
            assert!(!compatible(from, k(bad)), "7A → {bad} should not mix");
        }
        // round the top of the wheel
        assert!(compatible(k("12A"), k("2A")));
        assert!(compatible(k("1B"), k("11B")));
    }

    #[test]
    fn key_sorts_run_round_the_wheel_and_keep_unkeyed_last() {
        let items = vec![
            item(1, Some("9A"), Some(124.0)),
            item(2, None, Some(120.0)),
            item(3, Some("1B"), None),
            item(4, Some("1A"), Some(128.0)),
            item(5, Some("9A"), Some(122.0)),
        ];
        assert_eq!(by_key(&items, false), vec![4, 3, 5, 1, 2]);
        assert_eq!(by_key(&items, true), vec![5, 1, 3, 4, 2]);
    }

    #[test]
    fn bpm_sort_puts_missing_last() {
        let items = vec![
            item(1, None, Some(128.0)),
            item(2, None, None),
            item(3, None, Some(100.0)),
            item(4, None, Some(0.0)),
        ];
        assert_eq!(by_bpm(&items), vec![3, 1, 2, 4]);
    }

    #[test]
    fn random_is_a_permutation() {
        let items: Vec<Item> = (1..=50).map(|i| item(i, None, None)).collect();
        let out = sort_order(&items, SortMode::Random, &mut Rng::new(7));
        assert!(is_permutation(&items, &out));
        assert_ne!(out, (1..=50).collect::<Vec<_>>());
    }

    #[test]
    fn harmonic_walk_mixes_every_step_when_it_can() {
        // a chain that only mixes one way through: 1A 3A 5A 7A 7B 9B 11B
        let keys = ["7B", "1A", "11B", "5A", "3A", "9B", "7A"];
        let items: Vec<Item> =
            keys.iter().enumerate().map(|(i, s)| item(i as i64, Some(s), None)).collect();
        for seed in 1..20 {
            let out = sort_order(&items, SortMode::Harmonic, &mut Rng::new(seed));
            assert!(is_permutation(&items, &out));
            let key_of = |id: i64| items[id as usize].key.unwrap();
            for w in out.windows(2) {
                assert!(compatible(key_of(w[0]), key_of(w[1])), "seed {seed}: {out:?}");
            }
        }
    }

    #[test]
    fn harmonic_from_first_keeps_the_opener() {
        let items = vec![
            item(10, Some("4A"), None),
            item(11, Some("8A"), None),
            item(12, Some("5A"), None),
            item(13, None, None),
            item(14, Some("6A"), None),
            item(15, Some("7A"), None),
        ];
        for seed in 1..20 {
            let out = sort_order(&items, SortMode::HarmonicFromFirst, &mut Rng::new(seed));
            assert!(is_permutation(&items, &out));
            assert_eq!(out[0], 10);
            assert_eq!(*out.last().unwrap(), 13, "unkeyed last");
        }
    }

    #[test]
    fn harmonic_from_unkeyed_first_keeps_it_on_top() {
        let items = vec![item(1, None, None), item(2, Some("8A"), None), item(3, Some("8B"), None)];
        let out = sort_order(&items, SortMode::HarmonicFromFirst, &mut Rng::new(3));
        assert_eq!(out[0], 1);
        assert!(is_permutation(&items, &out));
    }

    #[test]
    fn harmonic_still_places_everything_when_keys_clash() {
        let items = vec![item(1, Some("1A"), None), item(2, Some("7B"), None), item(3, Some("4A"), None)];
        let out = sort_order(&items, SortMode::Harmonic, &mut Rng::new(9));
        assert!(is_permutation(&items, &out));
    }

    #[test]
    fn modes_parse() {
        for s in ["key_asc", "key_desc", "harmonic", "harmonic_first", "bpm", "random"] {
            assert!(SortMode::parse(s).is_some());
        }
        assert!(SortMode::parse("nope").is_none());
    }
}
