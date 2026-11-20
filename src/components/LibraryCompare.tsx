import { useCallback, useEffect, useRef, useState } from "react";
import * as api from "../api";
import ModalBackdrop from "./ModalBackdrop";

interface Props {
  /** A library, folder crate, Serato crate or Rekordbox playlist. */
  source: api.TrackSetRef;
  sourceName: string;
  onClose: () => void;
  onError: (message: string) => void;
}

const LEVELS: { key: api.MatchLevel; label: string }[] = [
  { key: "identical", label: "Identical file" },
  { key: "same_tags", label: "Same tags" },
  { key: "similar", label: "Similar" },
];

type DetailFilter = api.MatchLevel | "only_source" | "only_target" | null;

const DETAILS_LIMIT = 2000;

function pairName(t: api.PairTrack): string {
  if (t.title) return t.title;
  const file = t.rel_path.split("/").pop() ?? t.rel_path;
  return file.replace(/\.[^.]+$/, "");
}

export default function LibraryCompare({ source, sourceName, onClose, onError }: Props) {
  const [rows, setRows] = useState<api.SetComparison[] | null>(null);
  const [hashing, setHashing] = useState<api.HashProgress | null>(null);
  const [detailFor, setDetailFor] = useState<api.SetComparison | null>(null);
  const [filter, setFilter] = useState<DetailFilter>(null);
  const [details, setDetails] = useState<api.MatchPage | null>(null);
  const [unmatched, setUnmatched] = useState<api.Unmatched | null>(null);
  const [showAll, setShowAll] = useState(false);
  const key = api.setKey(source);
  // Held in a ref, not an effect dependency: App passes a fresh inline
  // onError each render, and a failing comparison (stick unplugged) would
  // otherwise re-run itself on every toast it shows.
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  const refresh = useCallback(() => {
    api
      .compareSet(JSON.parse(key))
      .then(setRows)
      .catch((e) => onErrorRef.current(String(e)));
  }, [key]);

  // Initial load + auto-start hashing so content matches fill in.
  useEffect(() => {
    refresh();
    // Everything unhashed, as before this window took sets: an "identical"
    // match needs a hash on both sides, and candidates span every library.
    api
      .startHashing(null)
      .then((n) => {
        if (n > 0) setHashing({ done: 0, failed: 0, total: n });
      })
      .catch(() => {
        // a hashing run may already be in progress — the events still arrive
      });
  }, [refresh]);

  useEffect(() => {
    const unsubs = [
      api.onHashProgress((p) => setHashing(p)),
      api.onHashDone(() => {
        setHashing(null);
        refresh();
      }),
    ];
    return () => {
      unsubs.forEach((u) => u.then((f) => f()).catch(() => {}));
    };
  }, [refresh]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (detailFor) setDetailFor(null);
        else onClose();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose, detailFor]);

  // Drill-down fetch; re-runs when the filter changes or hashing lands.
  useEffect(() => {
    setDetails(null);
    setUnmatched(null);
    if (!detailFor) return;
    const src = JSON.parse(key) as api.TrackSetRef;
    if (filter === "only_source" || filter === "only_target") {
      api
        .compareSetUnmatched(src, detailFor.target)
        .then(setUnmatched)
        .catch((e) => onErrorRef.current(String(e)));
    } else {
      api
        .compareSetDetails(src, detailFor.target, filter, DETAILS_LIMIT, 0)
        .then(setDetails)
        .catch((e) => onErrorRef.current(String(e)));
    }
  }, [detailFor, filter, key, rows]);

  const unhashed =
    rows?.reduce((n, r) => Math.max(n, r.unhashed_source + r.unhashed_target), 0) ?? 0;
  const hidden = rows?.filter((r) => r.shared === 0).length ?? 0;
  const shown = (rows ?? []).filter((r) => showAll || r.shared > 0);

  const levelChipClass = (level: api.MatchLevel) => (level === "same_tags" ? "tags" : level);

  const soloList = (list: api.PairTrack[], total: number) => (
    <>
      {list.length === 0 ? (
        <div className="modal-sub">Nothing here — every track has a match.</div>
      ) : (
        list.map((t) => (
          <div className="pair-row" key={t.id}>
            <div className="pair-track">
              <div className="name">{pairName(t)}</div>
              <div className="sub">
                {t.artist ?? "—"} · {t.rel_path}
              </div>
            </div>
          </div>
        ))
      )}
      {total > list.length && (
        <div className="modal-sub">
          Showing first {list.length.toLocaleString()} of {total.toLocaleString()}.
        </div>
      )}
    </>
  );

  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal compare-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title">
          {detailFor ? (
            <>
              <button className="btn back" onClick={() => setDetailFor(null)} data-tip="Back">
                ←
              </button>
              {sourceName} ↔ {detailFor.name}
            </>
          ) : (
            <>Similarity — {sourceName}</>
          )}
        </div>

        {hashing && hashing.total > 0 && (
          <div className="hash-progress">
            <div className="hash-bar">
              <div
                className="hash-fill"
                style={{
                  width: `${((hashing.done + hashing.failed) / hashing.total) * 100}%`,
                }}
              />
            </div>
            <span className="mono">
              hashing {hashing.done + hashing.failed}/{hashing.total}
            </span>
            <button
              className="pill-cancel"
              onClick={() => api.cancelHashing().catch(() => {})}
              data-tip="Cancel hashing"
            >
              ✕
            </button>
          </div>
        )}
        {!hashing && unhashed > 0 && (
          <div className="modal-sub">
            {unhashed.toLocaleString()} tracks not yet hashed — content matches may be
            incomplete.
          </div>
        )}

        {!detailFor ? (
          <>
            {hidden > 0 && (
              <div className="compare-filters">
                <span
                  className={`filter-chip ${showAll ? "active" : ""}`}
                  onClick={() => setShowAll((v) => !v)}
                >
                  Show all ({hidden} with no overlap)
                </span>
              </div>
            )}
            <div className="compare-list">
              {rows === null ? (
                <div className="modal-sub">Comparing…</div>
              ) : shown.length === 0 ? (
                <div className="modal-sub">Nothing else shares tracks with this.</div>
              ) : (
                shown.map((r) => (
                  <div
                    key={api.setKey(r.target)}
                    className="compare-row"
                    onClick={() => {
                      setFilter(null);
                      setDetailFor(r);
                    }}
                    data-tip="Show matched and missing tracks"
                  >
                    <div className="compare-name">
                      {r.name} <span className="count">{r.kind_label}</span>
                    </div>
                    <div className="compare-shared mono">{r.shared.toLocaleString()} shared</div>
                    <div className="compare-pct mono">{r.pct_of_source.toFixed(1)}%</div>
                    <div className="compare-breakdown">
                      <span className="chip identical mono">{r.identical} identical</span>
                      <span className="chip tags mono">{r.same_tags} same tags</span>
                      <span className="chip similar mono">{r.similar} similar</span>
                    </div>
                  </div>
                ))
              )}
            </div>
          </>
        ) : (
          <>
            <div className="compare-filters">
              <span
                className={`filter-chip ${filter === null ? "active" : ""}`}
                onClick={() => setFilter(null)}
              >
                All matches {details && filter === null ? `(${details.total})` : ""}
              </span>
              {LEVELS.map((l) => (
                <span
                  key={l.key}
                  className={`filter-chip ${filter === l.key ? "active" : ""}`}
                  onClick={() => setFilter(l.key)}
                >
                  {l.label}
                </span>
              ))}
              <span
                className={`filter-chip ${filter === "only_source" ? "active" : ""}`}
                onClick={() => setFilter("only_source")}
              >
                Only in {sourceName}
              </span>
              <span
                className={`filter-chip ${filter === "only_target" ? "active" : ""}`}
                onClick={() => setFilter("only_target")}
              >
                Only in {detailFor.name}
              </span>
            </div>
            <div className="compare-details">
              {filter === "only_source" || filter === "only_target" ? (
                unmatched === null ? (
                  <div className="modal-sub">Loading…</div>
                ) : filter === "only_source" ? (
                  soloList(unmatched.only_source, unmatched.only_source_total)
                ) : (
                  soloList(unmatched.only_target, unmatched.only_target_total)
                )
              ) : details === null ? (
                <div className="modal-sub">Loading…</div>
              ) : details.rows.length === 0 ? (
                <div className="modal-sub">No matches at this level.</div>
              ) : (
                details.rows.map((p) => (
                  <div className="pair-row" key={`${p.source.id}-${p.target.id}`}>
                    <div className="pair-track">
                      <div className="name">{pairName(p.source)}</div>
                      <div className="sub">
                        {p.source.artist ?? "—"} · {p.source.rel_path}
                      </div>
                    </div>
                    <span className={`chip ${levelChipClass(p.level)} mono`}>
                      {p.level === "identical"
                        ? "IDENTICAL"
                        : p.level === "same_tags"
                          ? "TAGS"
                          : "SIMILAR"}
                    </span>
                    <div className="pair-track">
                      <div className="name">{pairName(p.target)}</div>
                      <div className="sub">
                        {p.target.artist ?? "—"} · {p.target.rel_path}
                      </div>
                    </div>
                  </div>
                ))
              )}
              {filter !== "only_source" &&
                filter !== "only_target" &&
                details &&
                details.total > details.rows.length && (
                  <div className="modal-sub">
                    Showing first {details.rows.length.toLocaleString()} of{" "}
                    {details.total.toLocaleString()} matches.
                  </div>
                )}
            </div>
          </>
        )}

        <div className="modal-actions">
          <button className="btn" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
