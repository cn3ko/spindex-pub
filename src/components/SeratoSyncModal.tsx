import { useState } from "react";
import * as api from "../api";

interface Props {
  letter: string;
  file: string;
  name: string;
  plan: api.SeratoSyncPlan;
  onClose: () => void;
  onApplied: (res: api.SeratoEditResult) => void;
}

function label(e: api.SyncEntry): string {
  if (e.title) return e.artist ? `${e.artist} — ${e.title}` : e.title;
  return e.path;
}

/** Reconcile dialog for a Serato crate. "Add" pushes intended-but-missing
 *  tracks back into Serato; each Serato-only track is kept (adopted) or has
 *  its link removed — audio is never deleted. Serato must be closed. */
export default function SeratoSyncModal({
  letter,
  file,
  name,
  plan,
  onClose,
  onApplied,
}: Props) {
  // spindex_only: checked = add back (default all checked)
  const [add, setAdd] = useState<Set<string>>(
    () => new Set(plan.spindex_only.map((e) => e.path)),
  );
  // serato_only: checked = remove link (default none — Keep is the default)
  const [remove, setRemove] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);

  const toggle = (set: Set<string>, setSet: (s: Set<string>) => void, p: string) => {
    const next = new Set(set);
    if (next.has(p)) next.delete(p);
    else next.add(p);
    setSet(next);
  };

  const apply = async () => {
    setBusy(true);
    try {
      const res = await api.seratoSyncApply(
        letter,
        file,
        Array.from(add),
        Array.from(remove),
      );
      onApplied(res);
    } catch (e) {
      onApplied({ added: 0, removed: 0, copied: 0, skipped: 0, failed: 0, rescan_library: null });
      alert(String(e));
    } finally {
      setBusy(false);
    }
  };

  const nothing =
    plan.spindex_only.length === 0 && plan.serato_only.length === 0;

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal sync-modal" onClick={(e) => e.stopPropagation()}>
        <div className="sync-head">
          Sync “{name}” on {letter}
        </div>
        <div className="sync-note">
          Close Serato before syncing. Audio files are never deleted — only crate links change.
        </div>

        {nothing && <div className="sync-empty">Already in sync — nothing to reconcile.</div>}

        {plan.spindex_only.length > 0 && (
          <div className="sync-group">
            <div className="sync-group-title">
              In SpinDex, not in Serato — add back?
            </div>
            {plan.spindex_only.map((e) => (
              <label key={e.path} className="sync-row">
                <input
                  type="checkbox"
                  checked={add.has(e.path)}
                  onChange={() => toggle(add, setAdd, e.path)}
                />
                <span>{label(e)}</span>
              </label>
            ))}
          </div>
        )}

        {plan.serato_only.length > 0 && (
          <div className="sync-group">
            <div className="sync-group-title">
              In Serato, not in SpinDex — keep or remove the link?
            </div>
            {plan.serato_only.map((e) => (
              <div key={e.path} className="sync-row">
                <span className="sync-name">{label(e)}</span>
                <span className="sync-choice">
                  <button
                    className={`sync-pill ${!remove.has(e.path) ? "on" : ""}`}
                    onClick={() => remove.has(e.path) && toggle(remove, setRemove, e.path)}
                  >
                    Keep
                  </button>
                  <button
                    className={`sync-pill danger ${remove.has(e.path) ? "on" : ""}`}
                    onClick={() => !remove.has(e.path) && toggle(remove, setRemove, e.path)}
                  >
                    Remove link
                  </button>
                </span>
              </div>
            ))}
          </div>
        )}

        <div className="sync-actions">
          <button className="btn" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button className="btn primary" onClick={apply} disabled={busy || nothing}>
            {busy ? "Applying…" : "Apply"}
          </button>
        </div>
      </div>
    </div>
  );
}
