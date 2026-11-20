import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface LibraryInfo {
  id: number;
  name: string;
  root_path: string;
  is_removable: boolean;
  last_scanned_at: string | null;
  track_count: number;
  volume_serial: string | null;
}

export interface TrackRow {
  id: number;
  library_id: number;
  rel_path: string;
  title: string | null;
  artist: string | null;
  album: string | null;
  genre: string | null;
  label: string | null;
  year: number | null;
  bpm: number | null;
  key_camelot: string | null;
  rating: number | null;
  comment: string | null;
  duration_secs: number | null;
  bitrate_kbps: number | null;
  format: string | null;
  has_artwork: boolean;
  is_missing: boolean;
  /** epoch millis: when the track was first indexed */
  added_at: number | null;
  /** epoch millis as string: file mtime at last scan/edit */
  modified_at: string;
  /** 'manual' keys are user ground truth; analysis never overwrites them */
  key_source: "analysis" | "manual" | null;
  /** 0-1, mean winner probability of the segment-voted key passes */
  key_confidence: number | null;
  /** JSON array [{key, p}, …], voted winner first; parse with parseKeyCandidates */
  key_candidates: string | null;
  /** integrated loudness, LUFS (EBU R128); NULL until analyzed */
  loudness_lufs: number | null;
  /** fraction of samples in sustained clipping runs, 0-1 */
  clip_ratio: number | null;
  /** measured spectral cutoff in Hz — low values on lossless files mean transcode */
  cutoff_hz: number | null;
  /** 0-100 derived quality score; loudness deliberately not included */
  quality_score: number | null;
  /** ISO timestamp of the last enhance run that touched this file, or null */
  enhanced_at: string | null;
  /** 1-based manual position inside the track's own folder-crate */
  crate_pos: number | null;
  /** tempo wander as a % of the dominant tempo; NULL until analyzed */
  tempo_drift_pct: number | null;
  /** Who wrote the file's Serato grid: "auto", "manual", or null (never SpinDex). */
  serato_grid_source: string | null;
}

export interface KeyCandidate {
  key: string;
  /** mean probability across the vote passes, 0-1 */
  p: number;
}

export function parseKeyCandidates(raw: string | null): KeyCandidate[] {
  if (!raw) return [];
  try {
    const arr = JSON.parse(raw) as KeyCandidate[];
    // A candidate needs a key and a finite probability, or its tooltip reads "NaN%".
    return Array.isArray(arr)
      ? arr.filter((c) => typeof c?.key === "string" && typeof c.p === "number" && isFinite(c.p))
      : [];
  } catch {
    return [];
  }
}

export interface TrackPage {
  rows: TrackRow[];
  total: number;
}

export interface ScanProgress {
  library_id: number;
  found: number;
}

export interface ScanResult {
  library_id: number;
  found: number;
  new: number;
  updated: number;
  unchanged: number;
  errors: number;
  elapsed_secs: number;
}

export interface PlaybackInfo {
  path: string;
  transcoded: boolean;
}

export interface AnalysisProgress {
  done: number;
  failed: number;
  total: number;
}

export const listLibraries = () => invoke<LibraryInfo[]>("list_libraries");

export const addLibrary = (path: string) =>
  invoke<LibraryInfo>("add_library", { path });

export const scanLibrary = (libraryId: number) =>
  invoke<ScanResult>("scan_library", { libraryId });

export type HealthFilter =
  | "missing_bpm"
  | "missing_key"
  | "missing_artwork"
  | "missing_file"
  | "analysis_error"
  | "low_quality"
  | "unsteady_tempo";

export const listTracks = (
  query: string,
  libraryId: number | null,
  sort: string | null,
  descending: boolean,
  health: HealthFilter | null,
  folder: string | null,
  trackIds: number[] | null,
  limit: number,
  offset: number,
  /** Camelot codes ("8A"); a track passes when its key is any of them.
   *  null or empty = no key filter. */
  keys: string[] | null = null,
) =>
  invoke<TrackPage>("list_tracks", {
    query,
    libraryId,
    sort,
    descending,
    health,
    folder,
    trackIds,
    keys: keys && keys.length > 0 ? keys : null,
    limit,
    offset,
  });

export interface SeratoCrateInfo {
  name: string;
  /** file name inside _Serato_/Subcrates — the crate's identity for edits */
  file: string;
  track_ids: number[];
  /** crate entries not present in the index */
  missing: number;
}

/** Serato subcrates of a connected stick. */
export const listSeratoCrates = (letter: string) =>
  invoke<SeratoCrateInfo[]>("list_serato_crates", { letter });

export interface RekordboxCrateInfo {
  id: number;
  parent_id: number;
  name: string;
  is_folder: boolean;
  track_count: number;
  track_ids: number[];
  /** playlist entries that did not resolve to an indexed SpinDex track */
  missing: number;
}

/** Read-only view of a connected stick's Rekordbox playlists/folders,
 *  parsed from PIONEER/rekordbox/export.pdb. Not sorted — the tree
 *  structure comes from `parent_id` (0 = root), not array order. */
export const listRekordboxCrates = (letter: string) =>
  invoke<RekordboxCrateInfo[]>("list_rekordbox_crates", { letter });

/** Playlists of the XML SpinDex exported to the stick (SpinDex/rekordbox.xml)
 *  -- prepared for Rekordbox, not yet imported by it. Empty when no export
 *  exists. Same shape as listRekordboxCrates; ids are the XML's own. */
export const listPreparedRekordbox = (letter: string) =>
  invoke<RekordboxCrateInfo[]>("list_prepared_rekordbox", { letter });

export interface SeratoEditResult {
  added: number;
  removed: number;
  copied: number;
  skipped: number;
  failed: number;
  rescan_library: number | null;
}

/** Adds tracks to a Serato crate; foreign tracks are copied to the stick
 *  first. Crate file backed up first-touch, written atomically. */
export const seratoAddTracks = (
  letter: string,
  file: string,
  trackIds: number[],
  beforeIndex: number | null = null,
) => invoke<SeratoEditResult>("serato_add_tracks", { letter, file, trackIds, beforeIndex });

/** Removes link entries from a Serato crate; audio files stay untouched. */
export const seratoRemoveTracks = (letter: string, file: string, trackIds: number[]) =>
  invoke<number>("serato_remove_tracks", { letter, file, trackIds });

/** Drops the selection before `beforeIndex` in the Serato crate's own order. */
export const seratoReorderTracks = (
  letter: string,
  file: string,
  trackIds: number[],
  beforeIndex: number,
) => invoke<void>("serato_reorder_tracks", { letter, file, trackIds, beforeIndex });

/** Moves the selection one slot up (delta -1) or down (delta +1). */
export const seratoMoveTracks = (
  letter: string,
  file: string,
  trackIds: number[],
  delta: number,
) => invoke<void>("serato_move_tracks", { letter, file, trackIds, delta });

export const seratoCreateCrate = (letter: string, name: string) =>
  invoke<void>("serato_create_crate", { letter, name });

/** Returns the crate's new file name. */
export const seratoRenameCrate = (letter: string, file: string, newName: string) =>
  invoke<string>("serato_rename_crate", { letter, file, newName });

/** Deletes the .crate link file (backed up first); never audio. */
export const seratoDeleteCrate = (letter: string, file: string) =>
  invoke<void>("serato_delete_crate", { letter, file });

export interface SyncEntry {
  path: string;
  title: string | null;
  artist: string | null;
}

export interface SeratoSyncPlan {
  /** intended, missing from Serato — offer to add back */
  spindex_only: SyncEntry[];
  /** in Serato, not intended — offer keep or remove-link */
  serato_only: SyncEntry[];
}

/** Previews the reconcile between SpinDex intent and the live .crate. Errors
 *  (rendered by the caller) when Serato is running. */
export const seratoSyncPreview = (letter: string, file: string) =>
  invoke<SeratoSyncPlan>("serato_sync_preview", { letter, file });

/** Applies reconcile choices: add intended paths, remove link entries (audio
 *  never touched); intent is rewritten to match afterward. */
export const seratoSyncApply = (
  letter: string,
  file: string,
  add: string[],
  remove: string[],
) => invoke<SeratoEditResult>("serato_sync_apply", { letter, file, add, remove });

export interface CrateFolder {
  /** forward-slash path relative to the library root, e.g. "House/Deep House" */
  path: string;
  /** last path segment, the display name */
  name: string;
  /** nesting level: "House" = 0, "House/Deep House" = 1 */
  depth: number;
  /** tracks directly in this folder (0 for purely intermediate folders) */
  track_count: number;
}

/** Folder-crates of a library, derived from subfolders, hierarchical order. */
export const listCrates = (libraryId: number) =>
  invoke<CrateFolder[]>("list_crates", { libraryId });

/** Creates a crate folder on disk; returns its rel path. */
export const createCrate = (libraryId: number, parentPath: string, name: string) =>
  invoke<string>("create_crate", { libraryId, parentPath, name });

/** Renames the crate folder and re-points its tracks; returns the new path. */
export const renameCrate = (libraryId: number, path: string, newName: string) =>
  invoke<string>("rename_crate", { libraryId, path, newName });

/** Deletes an empty crate folder (non-empty folders are refused). */
export const deleteCrate = (libraryId: number, path: string) =>
  invoke<void>("delete_crate", { libraryId, path });

export interface MoveResult {
  moved: number;
  skipped: number;
  failed: number;
}

/** Moves tracks of one library into a crate folder ("" = library root). */
export const moveTracksToFolder = (
  trackIds: number[],
  libraryId: number,
  folder: string,
) => invoke<MoveResult>("move_tracks_to_folder", { trackIds, libraryId, folder });

/** Moves the selection one slot up (delta -1) or down (delta +1) in a crate. */
export const moveCrateTracks = (
  trackIds: number[],
  libraryId: number,
  folder: string,
  delta: number,
) => invoke<void>("move_crate_tracks", { trackIds, libraryId, folder, delta });

/** Drops the selection before `beforeIndex` in the crate's current order. */
export const reorderCrateTracks = (
  trackIds: number[],
  libraryId: number,
  folder: string,
  beforeIndex: number,
) => invoke<void>("reorder_crate_tracks", { trackIds, libraryId, folder, beforeIndex });

/** The toolbar's Sort menu: how a whole crate gets re-numbered. */
export type CrateSortMode =
  | "key_asc"
  | "key_desc"
  | "harmonic"
  | "harmonic_first"
  | "bpm"
  | "random";

export const CRATE_SORT_LABELS: Record<CrateSortMode, string> = {
  key_asc: "Key low to high",
  key_desc: "Key high to low",
  harmonic: "Harmonic mix, random start",
  harmonic_first: "Harmonic mix from the first track",
  bpm: "BPM low to high",
  random: "Random",
};

/** Re-numbers a whole folder-crate once (the order file is rewritten). */
export const sortCrateTracks = (libraryId: number, folder: string, mode: CrateSortMode) =>
  invoke<void>("sort_crate_tracks", { libraryId, folder, mode });

/** Re-numbers a whole Serato crate once (the .crate file is rewritten). */
export const seratoSortTracks = (letter: string, file: string, mode: CrateSortMode) =>
  invoke<void>("serato_sort_tracks", { letter, file, mode });

export interface AddToCrateResult {
  moved: number;
  copied: number;
  skipped: number;
  failed: number;
}

/** Split view drop: tracks land in a folder-crate before row `beforeIndex`.
 *  Same library moves the files in, other libraries copy them. */
export const addTracksToCrate = (
  trackIds: number[],
  libraryId: number,
  folder: string,
  beforeIndex: number,
) =>
  invoke<AddToCrateResult>("add_tracks_to_crate", { trackIds, libraryId, folder, beforeIndex });

export interface DeviceInfo {
  letter: string;
  label: string;
  serial: string | null;
  known_library_id: number | null;
}

export interface DeviceArrived {
  letter: string;
  label: string;
  known_library_ids: number[];
}

export interface DeviceRemoved {
  letter: string;
  library_ids: number[];
}

export interface HealthCounts {
  duplicates: number;
  missing_bpm: number;
  missing_key: number;
  missing_artwork: number;
  missing_file: number;
  analysis_error: number;
  low_quality: number;
  unsteady_tempo: number;
}

export interface DupeTrackInfo {
  id: number;
  library_id: number;
  title: string | null;
  artist: string | null;
  rel_path: string;
  bitrate_kbps: number | null;
  format: string | null;
  file_size: number;
  duration_secs: number | null;
  quality_score: number | null;
}

export interface DupeGroup {
  level: "identical" | "same_tags" | "similar" | "same_filename";
  tracks: DupeTrackInfo[];
  suggested_keeper_id: number | null;
}

export interface DupeReport {
  groups: DupeGroup[];
  unhashed: number;
}

export const listDevices = () => invoke<DeviceInfo[]>("list_devices");

export const addDeviceLibraries = (letter: string) =>
  invoke<LibraryInfo[]>("add_device_libraries", { letter });

/** Safe-removal of the stick at this drive letter ("E:"). */
export const ejectDevice = (letter: string) =>
  invoke<void>("eject_device", { letter });

/** Drops a stick's libraries + index by volume serial; files are untouched. */
export const forgetDevice = (serial: string) =>
  invoke<number>("forget_device", { serial });

/** Unlinks one library from the index; files on disk are untouched. */
export const removeLibrary = (libraryId: number) =>
  invoke<void>("remove_library", { libraryId });

export const findDuplicates = (libraryId: number) =>
  invoke<DupeReport>("find_duplicates", { libraryId });

export const libraryHealth = (libraryId: number | null) =>
  invoke<HealthCounts>("library_health", { libraryId });

export interface QualityDetail {
  loudness_lufs: number | null;
  clip_ratio: number | null;
  cutoff_hz: number | null;
  quality_score: number | null;
  /** 256x128 u8 dB-scaled spectrogram, row-major [time][band], band 0 lowest */
  spectrogram: number[] | null;
  sample_rate: number | null;
  bitrate_kbps: number | null;
  format: string | null;
}

export const qualityDetail = (trackId: number) =>
  invoke<QualityDetail>("quality_detail", { trackId });

export interface GenreCount {
  genre: string;
  count: number;
}

/** Distinct genres across all libraries, most-used first (case-insensitively deduped). */
export const listGenres = () => invoke<GenreCount[]>("list_genres");

export const onDeviceArrived = (
  cb: (d: DeviceArrived) => void,
): Promise<UnlistenFn> => listen<DeviceArrived>("device:arrived", (e) => cb(e.payload));

export const onDeviceRemoved = (
  cb: (d: DeviceRemoved) => void,
): Promise<UnlistenFn> => listen<DeviceRemoved>("device:removed", (e) => cb(e.payload));

export interface TagUpdate {
  title: string | null;
  artist: string | null;
  album: string | null;
  genre: string | null;
  year: number | null;
  bpm: number | null;
  key: string | null;
  comment: string | null;
  label: string | null;
  /** 1-5 stars; null clears the rating */
  rating: number | null;
}

/** Writes tags into the audio file, then updates the cache row. */
export const updateTrackMetadata = (trackId: number, update: TagUpdate) =>
  invoke<void>("update_track_metadata", { trackId, update });

/** Path to a temp file with the track's embedded cover, or null. */
export const artworkPath = (trackId: number) =>
  invoke<string | null>("artwork_path", { trackId });

/** Embeds a jpg/png file as the track's front cover (written to the file). */
export const setArtwork = (trackId: number, imagePath: string) =>
  invoke<void>("set_artwork", { trackId, imagePath });

export const removeArtwork = (trackId: number) =>
  invoke<void>("remove_artwork", { trackId });

export type MatchSource = "text_search" | "none";

export interface EnrichFields {
  title: string | null;
  artist: string | null;
  album: string | null;
  year: number | null;
  label: string | null;
  genre: string | null;
}

export interface EnrichResult {
  source: MatchSource;
  loose: boolean;
  fields: EnrichFields;
}

/** Online metadata lookup for a track. Read-only; writes nothing. */
export const fetchOnlineMetadata = (trackId: number) =>
  invoke<EnrichResult>("fetch_online_metadata", { trackId });

export const analyzerAvailable = () => invoke<boolean>("analyzer_available");

/** trackIds null = everything in scope; libraryId null = all libraries. */
export const startAnalysis = (
  trackIds: number[] | null,
  libraryId: number | null,
  overwrite: boolean,
) => invoke<number>("start_analysis", { trackIds, libraryId, overwrite });

export const cancelAnalysis = () => invoke<void>("cancel_analysis");

export const preparePlayback = (trackId: number) =>
  invoke<PlaybackInfo>("prepare_playback", { trackId });

export const getWaveform = (trackId: number) =>
  invoke<number[] | null>("get_waveform", { trackId });

export const getWaveformBands = (trackId: number) =>
  invoke<number[] | null>("waveform_bands", { trackId });

/** Stored (sampled) beat times in seconds; null until analyzed. */
export const beatGrid = (trackId: number) =>
  invoke<number[] | null>("beat_grid", { trackId });

/** Stored (sampled) downbeat times in seconds; null until analyzed. */
export const downbeatGrid = (trackId: number) =>
  invoke<number[] | null>("downbeat_grid", { trackId });

export const markMissing = (trackId: number) =>
  invoke<void>("mark_missing", { trackId });

export const onAnalysisProgress = (
  cb: (p: AnalysisProgress) => void,
): Promise<UnlistenFn> =>
  listen<AnalysisProgress>("analysis:progress", (e) => cb(e.payload));

export const onAnalysisDone = (
  cb: (p: AnalysisProgress) => void,
): Promise<UnlistenFn> =>
  listen<AnalysisProgress>("analysis:done", (e) => cb(e.payload));

export interface LibraryComparison {
  library_id: number;
  name: string;
  source_total: number;
  shared: number;
  /** 0–100 */
  pct_of_source: number;
  identical: number;
  same_tags: number;
  similar: number;
  unhashed_source: number;
  unhashed_target: number;
}

export interface PairTrack {
  id: number;
  title: string | null;
  artist: string | null;
  rel_path: string;
  bitrate_kbps: number | null;
  format: string | null;
}

export type MatchLevel = "identical" | "same_tags" | "similar";

export interface MatchPair {
  source: PairTrack;
  target: PairTrack;
  level: MatchLevel;
}

export interface MatchPage {
  rows: MatchPair[];
  total: number;
}

export interface HashProgress {
  done: number;
  failed: number;
  total: number;
}

/** Ranked overlap of every other library against the given one. */
export const compareLibrary = (libraryId: number) =>
  invoke<LibraryComparison[]>("compare_library", { libraryId });

export const compareDetails = (
  sourceId: number,
  targetId: number,
  level: MatchLevel | null,
  limit: number,
  offset: number,
) =>
  invoke<MatchPage>("compare_details", { sourceId, targetId, level, limit, offset });

/** A library, folder crate, Serato crate or Rekordbox playlist. */
export type TrackSetRef =
  | { kind: "library"; id: number }
  | { kind: "folder"; library_id: number; path: string }
  | { kind: "serato"; letter: string; file: string }
  | { kind: "rekordbox"; letter: string; playlist_id: number };

export interface SetComparison {
  target: TrackSetRef;
  name: string;
  kind_label: string;
  source_total: number;
  shared: number;
  pct_of_source: number;
  identical: number;
  same_tags: number;
  similar: number;
  unhashed_source: number;
  unhashed_target: number;
}

export interface Unmatched {
  only_source: PairTrack[];
  only_target: PairTrack[];
  only_source_total: number;
  only_target_total: number;
}

/** Every other set ranked by how much of `source` it holds. */
export const compareSet = (source: TrackSetRef) =>
  invoke<SetComparison[]>("compare_set", { source });
export const compareSetDetails = (
  source: TrackSetRef,
  target: TrackSetRef,
  level: MatchLevel | null,
  limit: number,
  offset: number,
) => invoke<MatchPage>("compare_set_details", { source, target, level, limit, offset });
/** Tracks with no match at any level, on each side. */
export const compareSetUnmatched = (source: TrackSetRef, target: TrackSetRef) =>
  invoke<Unmatched>("compare_set_unmatched", { source, target });
export const findDuplicatesInSet = (set: TrackSetRef) =>
  invoke<DupeReport>("find_duplicates_in_set", { set });

/** Stable string key for a set, for React keys and equality checks. */
export const setKey = (s: TrackSetRef) => JSON.stringify(s);

/** libraryIds null = hash everything unhashed. Returns the queued count. */
export const startHashing = (libraryIds: number[] | null) =>
  invoke<number>("start_hashing", { libraryIds });

export const cancelHashing = () => invoke<void>("cancel_hashing");

export const onHashProgress = (
  cb: (p: HashProgress) => void,
): Promise<UnlistenFn> => listen<HashProgress>("hash:progress", (e) => cb(e.payload));

export const onHashDone = (
  cb: (p: HashProgress) => void,
): Promise<UnlistenFn> => listen<HashProgress>("hash:done", (e) => cb(e.payload));

export const onScanProgress = (
  cb: (p: ScanProgress) => void,
): Promise<UnlistenFn> => listen<ScanProgress>("scan:progress", (e) => cb(e.payload));

export const onScanDone = (
  cb: (r: ScanResult) => void,
): Promise<UnlistenFn> => listen<ScanResult>("scan:done", (e) => cb(e.payload));

export interface CopyProgress {
  done: number;
  skipped: number;
  failed: number;
  total: number;
}

/** Creates parent/name as a fresh folder and registers it as a library. */
export const createLibrary = (parent: string, name: string) =>
  invoke<LibraryInfo>("create_library", { parent, name });

/** Copies tracks flat into the target library (optionally into one of its
 *  crate folders); never overwrites. */
export const copyTracks = (
  trackIds: number[],
  targetLibraryId: number,
  destFolder?: string,
) =>
  invoke<number>("copy_tracks", {
    trackIds,
    targetLibraryId,
    destFolder: destFolder ?? null,
  });

export const cancelCopy = () => invoke<void>("cancel_copy");

/** Imports one OS-dropped file's bytes into a library (optionally into one of
 *  its crate folders); never overwrites. The webview never sees a dropped
 *  file's path, so the bytes travel as the raw invoke body and the metadata
 *  rides in URI-encoded headers (headers are ASCII-only). */
export const importFile = (
  libraryId: number,
  folder: string,
  filename: string,
  data: Uint8Array,
) =>
  invoke<"copied" | "skipped">("import_file", data, {
    // Empty-valued headers are silently dropped by the webview's HTTP layer,
    // so a root drop (folder "") must OMIT x-folder — the backend treats a
    // missing x-folder as the library root.
    headers: {
      "x-library-id": String(libraryId),
      ...(folder ? { "x-folder": encodeURIComponent(folder) } : {}),
      "x-filename": encodeURIComponent(filename),
    },
  });

/** Copies a whole crate folder (incl. subcrates) into another library:
 *  <target root>/<crate name>/… — existing files skipped (merge), never
 *  overwritten. Progress via the existing copy:* events. */
export const copyCrate = (
  sourceLibraryId: number,
  folder: string,
  targetLibraryId: number,
) => invoke<number>("copy_crate", { sourceLibraryId, folder, targetLibraryId });

export const onCopyProgress = (
  cb: (p: CopyProgress) => void,
): Promise<UnlistenFn> => listen<CopyProgress>("copy:progress", (e) => cb(e.payload));

export const onCopyDone = (
  cb: (p: CopyProgress) => void,
): Promise<UnlistenFn> => listen<CopyProgress>("copy:done", (e) => cb(e.payload));

export interface BatchProgress {
  done: number;
  failed: number;
  total: number;
}

export interface BatchDone extends BatchProgress {
  failed_names: string[];
  /** Files a repair sweep checked and left alone; 0 for a batch edit. */
  skipped: number;
}

/** Re-saves the tags of every track in a folder-crate that needs repairing,
 *  leaving files that are already correct untouched. Progress via batch:*. */
export const repairFolderCrateTags = (libraryId: number, folder: string) =>
  invoke<number>("repair_folder_crate_tags", { libraryId, folder });

/** Same sweep over the tracks a Serato crate links to. */
export const repairSeratoCrateTags = (letter: string, file: string) =>
  invoke<number>("repair_serato_crate_tags", { letter, file });

/** Builds and writes a Serato beat grid for the given tracks from SpinDex's
 *  analysis. Progress via batch:* events. `rewriteAll` re-writes tracks that
 *  already have a SpinDex grid. Returns the number queued. */
/** One stretch of a grid at one tempo (grid_edit::GridSection). */
export interface GridSection {
  start_sec: number;
  bpm: number;
}
export interface TrackGrid {
  /** at least one; more where the tempo changes */
  sections: GridSection[];
  source: "file" | "detected" | "bpm" | "none";
  file_unreadable: boolean;
}

/** The grid the deck's grid editor starts from. */
export const trackGrid = (trackId: number) => invoke<TrackGrid>("track_grid", { trackId });
/** The grid editor's waveform, decoded from the backend's raw bytes. Needs ffmpeg. */
export interface GridWave {
  /** milliseconds covered */
  ms: number;
  /** per ms [peak, low, mid, high], each 0-255; bands relative to their own loudest ms */
  bands: Uint8Array;
  /** per quarter ms [min, max], signed, square-root scale of the loudest sample (±127) */
  shape: Int8Array;
}
/** The grid editor's drum and harmony colourings, fetched after the waveform. */
export interface GridColours {
  ms: number;
  /** per ms [kick, hats, tonal], each 0-255 relative to its own loudest moment */
  drums: Uint8Array;
  /** per ms [camelot, confidence]: camelot = (number - 1) * 2 + (B ? 1 : 0), 255 = no chord */
  harmony: Uint8Array;
}
/** Reads a u32 LE millisecond count, then one block of `size` bytes per ms for each size. */
function msBlocks(b: ArrayBuffer, sizes: number[]): { ms: number; blocks: Uint8Array[] } {
  const ms = new DataView(b).getUint32(0, true);
  let at = 4;
  const blocks = sizes.map((size) => {
    const v = new Uint8Array(b, at, ms * size);
    at += ms * size;
    return v;
  });
  return { ms, blocks };
}
/** Layout: grid_edit::detail_payload (u32 ms, bands 4/ms, shape 8/ms). */
export const gridWaveform = (trackId: number) =>
  invoke<ArrayBuffer>("grid_waveform", { trackId }).then((b): GridWave => {
    const { ms, blocks } = msBlocks(b, [4, 8]);
    const s = blocks[1];
    return { ms, bands: blocks[0], shape: new Int8Array(s.buffer, s.byteOffset, s.length) };
  });
/** Layout: grid_edit::colour_payload (u32 ms, drums 3/ms, harmony 2/ms). Ask
 *  after gridWaveform: it reuses that call's decode. */
export const gridColours = (trackId: number) =>
  invoke<ArrayBuffer>("grid_colours", { trackId }).then((b): GridColours => {
    const { ms, blocks } = msBlocks(b, [3, 2]);
    return { ms, drums: blocks[0], harmony: blocks[1] };
  });
/** Writes a constant grid into the file and marks it manual. */
export const saveTrackGrid = (trackId: number, sections: GridSection[]) =>
  invoke<void>("save_track_grid", { trackId, sections });
/** File formats the grid can be written to (lower-case extensions). */
export const GRID_FORMATS = ["mp3", "wav", "aif", "aiff", "flac"];

export const regenerateSeratoGrid = (trackIds: number[], rewriteAll = false) =>
  invoke<number>("regenerate_serato_grid", { trackIds, rewriteAll });

export const regenerateFolderCrateGrids = (
  libraryId: number,
  folder: string,
  rewriteAll = false,
) =>
  invoke<number>("regenerate_folder_crate_grids", {
    libraryId,
    folder,
    rewriteAll,
  });

export const regenerateSeratoCrateGrids = (
  letter: string,
  file: string,
  rewriteAll = false,
) => invoke<number>("regenerate_serato_crate_grids", { letter, file, rewriteAll });

/** Applies a partial update to many tracks; only `apply` field names are
 *  written. Returns the total to process; progress via batch:* events. */
export const updateTracksMetadata = (
  trackIds: number[],
  update: TagUpdate,
  apply: string[],
) => invoke<number>("update_tracks_metadata", { trackIds, update, apply });

export const cancelBatch = () => invoke<void>("cancel_batch");

export const onBatchProgress = (
  cb: (p: BatchProgress) => void,
): Promise<UnlistenFn> => listen<BatchProgress>("batch:progress", (e) => cb(e.payload));

export const onBatchDone = (
  cb: (p: BatchDone) => void,
): Promise<UnlistenFn> => listen<BatchDone>("batch:done", (e) => cb(e.payload));

export interface SyncProgress { done: number; total: number }
export interface SyncLoss { what: string; tracks: number }
export interface SyncDone {
  ok: boolean;
  cancelled: boolean;
  error: string | null;
  path: string | null;
  tracks: number;
  playlists: number;
  grids: number;
  cues: number;
  loops: number;
  unreadable: string[];
  dangling: number;
  losses: SyncLoss[];
}
export interface SyncPreview {
  tracks: number;
  crates: number;
  out_path: string;
  capability_losses: string[];
}

/** Counts and expected losses for exporting a stick to Rekordbox XML. */
export const syncPreview = (letter: string, crates: string[] | null = null) =>
  invoke<SyncPreview>("sync_preview", { letter, crates });
/** Starts the Serato → Rekordbox XML export; progress via sync:* events. */
export const syncToRekordbox = (letter: string, crates: string[] | null = null) =>
  invoke<number>("sync_to_rekordbox", { letter, crates });
export const cancelSync = () => invoke<void>("cancel_sync");
export const onSyncProgress = (cb: (p: SyncProgress) => void): Promise<UnlistenFn> =>
  listen<SyncProgress>("sync:progress", (e) => cb(e.payload));
export const onSyncDone = (cb: (p: SyncDone) => void): Promise<UnlistenFn> =>
  listen<SyncDone>("sync:done", (e) => cb(e.payload));

/** What importing a Rekordbox XML onto a stick would write (only filling gaps). */
export interface RekordboxImportPreview {
  tracks_in_xml: number;
  matched: number;
  not_on_stick: number;
  /** matched files whose Serato data could not be read: left untouched */
  unreadable: number;
  /** matched files in a format SpinDex cannot write Serato data into (m4a, …) */
  unsupported: number;
  grids: number;
  cues: number;
  loops: number;
  colours: number;
  cues_skipped: number;
  loops_skipped: number;
  memory_cues_dropped: number;
  crates_new: string[];
  crates_merged: string[];
  links_added: number;
  /** playlist entries whose track is not on this stick (crates come out shorter) */
  playlist_entries_missing: number;
  /** bytes needed on the stick for first-touch backups plus one working copy */
  space_needed: number;
  /** bytes free on the stick; null when unknown */
  free_space: number | null;
}

export interface RekordboxImportProgress {
  done: number;
  total: number;
}

export interface RekordboxImportDone {
  files_written: number;
  failed_names: string[];
  crates_created: number;
  crates_merged: number;
  links_added: number;
  cancelled: boolean;
  error: string | null;
}

export const rekordboxImportPreview = (letter: string, xmlPath: string) =>
  invoke<RekordboxImportPreview>("rekordbox_import_preview", { letter, xmlPath });
/** Starts the import; progress via rbimport:* events. */
export const rekordboxImport = (letter: string, xmlPath: string) =>
  invoke<number>("rekordbox_import", { letter, xmlPath });
export const cancelRekordboxImport = () => invoke<void>("cancel_rekordbox_import");
export const onRekordboxImportProgress = (
  cb: (p: RekordboxImportProgress) => void,
): Promise<UnlistenFn> => listen<RekordboxImportProgress>("rbimport:progress", (e) => cb(e.payload));
export const onRekordboxImportDone = (cb: (p: RekordboxImportDone) => void): Promise<UnlistenFn> =>
  listen<RekordboxImportDone>("rbimport:done", (e) => cb(e.payload));

export interface DeleteReport {
  deleted: number;
  failed: string[];
}

/** Recycle-Bin delete (permanent on USB sticks); rows dropped only for files
 *  actually gone. The confirm dialog lives in the caller. */
export const deleteTracks = (trackIds: number[]) =>
  invoke<DeleteReport>("delete_tracks", { trackIds });

/** Opens Explorer with the track's file selected. */
export const showInExplorer = (trackId: number) =>
  invoke<void>("show_in_explorer", { trackId });

/** Reveal an absolute path in Explorer (for files without a track row yet). */
export const revealPath = (path: string) =>
  invoke<void>("reveal_path", { path });

export type EnhanceMode = "new_file" | "replace";
export type EnhanceFormat = "keep" | "flac";

/** Loudness-normalizes (target -9 LUFS) and optionally transcodes tracks;
 *  "new_file" writes an enhanced copy alongside the original, "replace"
 *  overwrites the file in place (first-touch backup, never destructive).
 *  Returns the queued count; progress via enhance:* events. */
export const enhanceTracks = (
  trackIds: number[],
  mode: EnhanceMode,
  format: EnhanceFormat,
  destDir: string | null = null,
) => invoke<number>("enhance_tracks", { trackIds, mode, format, destDir });

export const cancelEnhance = () => invoke("cancel_enhance");

/** Restores tracks previously enhanced with mode "replace" from their backup.
 *  Returns the names of tracks that failed to restore. */
export const restoreTracks = (trackIds: number[]) =>
  invoke<string[]>("restore_tracks", { trackIds });

export interface EnhanceProgress {
  done: number;
  failed: number;
  skipped: number;
  total: number;
}

export interface EnhanceDone extends EnhanceProgress {
  failed_names: string[];
  enhanced_ids: number[];
  library_ids: number[];
  /** absolute paths of files created by new_file mode this run */
  new_files: string[];
  /** per-skipped-track "name: reason" lines */
  skipped_names: string[];
}

export const onEnhanceProgress = (
  cb: (p: EnhanceProgress) => void,
): Promise<UnlistenFn> => listen<EnhanceProgress>("enhance:progress", (e) => cb(e.payload));

export const onEnhanceDone = (
  cb: (p: EnhanceDone) => void,
): Promise<UnlistenFn> => listen<EnhanceDone>("enhance:done", (e) => cb(e.payload));

/** Time-stretches tracks onto a constant beat grid (pitch preserved), reusing
 *  the same mode/format/destination machinery as enhanceTracks. `targetBpm`
 *  null means each track keeps its own detected tempo — only its drift is
 *  flattened. Returns the queued count; progress via stabilize:* events. */
export const stabilizeTracks = (
  trackIds: number[],
  targetBpm: number | null,
  mode: EnhanceMode,
  format: EnhanceFormat,
  destDir: string | null = null,
) => invoke<number>("stabilize_tracks", { trackIds, targetBpm, mode, format, destDir });

export const cancelStabilize = () => invoke("cancel_stabilize");

export const onStabilizeProgress = (
  cb: (p: EnhanceProgress) => void,
): Promise<UnlistenFn> => listen<EnhanceProgress>("stabilize:progress", (e) => cb(e.payload));

// stabilize:done reuses the EnhanceDone payload shape.
export const onStabilizeDone = (
  cb: (p: EnhanceDone) => void,
): Promise<UnlistenFn> => listen<EnhanceDone>("stabilize:done", (e) => cb(e.payload));

/** Whether the ML restoration sidecar (Apollo) and its model are present. */
export function apolloReady(): Promise<boolean> {
  return invoke("apollo_ready");
}

/** ML-restores tracks (denoise/dereverb/upsample via the Apollo sidecar),
 *  reusing the same mode/format choices as enhanceTracks. Returns the queued
 *  count; progress via mlrestore:* events. */
export function mlRestoreTracks(
  trackIds: number[],
  mode: EnhanceMode,
  format: EnhanceFormat,
  destDir: string | null = null,
): Promise<number> {
  return invoke("ml_restore_tracks", { trackIds, mode, format, destDir });
}

export function cancelMlRestore(): Promise<void> {
  return invoke("cancel_ml_restore");
}

export interface MlRestoreProgress {
  done: number;
  failed: number;
  skipped: number;
  total: number;
  chunk_done: number;
  chunk_total: number;
  current: string;
}

// mlrestore:done reuses the EnhanceDone payload shape.

export const onMlRestoreProgress = (
  cb: (p: MlRestoreProgress) => void,
): Promise<UnlistenFn> =>
  listen<MlRestoreProgress>("mlrestore:progress", (e) => cb(e.payload));

export const onMlRestoreDone = (
  cb: (p: EnhanceDone) => void,
): Promise<UnlistenFn> => listen<EnhanceDone>("mlrestore:done", (e) => cb(e.payload));
