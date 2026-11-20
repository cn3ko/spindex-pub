import { useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import * as api from "../api";

// Session cache of resolved artwork paths, keyed by `${trackId}:${artToken}`.
// A cached null means "no artwork / unreadable" — render the placeholder.
const pathCache = new Map<string, string | null>();
const inflight = new Map<string, Promise<string | null>>();

function resolveArtwork(trackId: number, artToken: number): Promise<string | null> {
  const key = `${trackId}:${artToken}`;
  if (pathCache.has(key)) return Promise.resolve(pathCache.get(key)!);
  const existing = inflight.get(key);
  if (existing) return existing;
  const p = api.artworkPath(trackId).then(
    (path) => {
      pathCache.set(key, path);
      inflight.delete(key);
      return path;
    },
    () => {
      inflight.delete(key); // transient error: do NOT cache, allow retry on next mount
      return null;
    },
  );
  inflight.set(key, p);
  return p;
}

const PLACEHOLDER = (
  <span className="row-thumb-placeholder" aria-hidden>
    <svg viewBox="0 0 24 24" width="12" height="12">
      <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" strokeWidth="2" />
      <circle cx="12" cy="12" r="2.5" fill="currentColor" />
    </svg>
  </span>
);

/**
 * Small album-art thumbnail for a track row. Fetches (once per trackId:artToken)
 * only when the track has embedded art; every failure falls back to a muted disc.
 */
export default function TrackThumb({
  trackId,
  hasArtwork,
  artToken,
}: {
  trackId: number;
  hasArtwork: boolean;
  artToken: number;
}) {
  const [src, setSrc] = useState<string | null>(null);
  const key = `${trackId}:${artToken}`;

  useEffect(() => {
    if (!hasArtwork) {
      setSrc(null);
      return;
    }
    let stale = false;
    const timer = setTimeout(() => {
      resolveArtwork(trackId, artToken).then((path) => {
        if (stale) return;
        setSrc(path ? `${convertFileSrc(path)}?v=${trackId}-${artToken}` : null);
      });
    }, 120);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [trackId, hasArtwork, artToken]);

  if (!hasArtwork || !src) return PLACEHOLDER;
  return (
    <img
      className="row-thumb"
      src={src}
      alt=""
      draggable={false}
      onError={() => {
        pathCache.delete(key);
        setSrc(null);
      }}
    />
  );
}
