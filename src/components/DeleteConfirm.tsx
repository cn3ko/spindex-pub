import { useEffect } from "react";
import ModalBackdrop from "./ModalBackdrop";

/** One track the user asked to delete, as the confirm needs to show it. */
export interface DeleteItem {
  name: string;
  /** On a removable drive: no Recycle Bin there, the file is gone for good. */
  onStick: boolean;
  /** Already gone from disk: only the library entry is removed. */
  missing: boolean;
}

const SHOWN = 5;

/** "Artist — Title", falling back to the file name when tags are empty. */
export function trackDisplayName(t: {
  title: string | null;
  artist: string | null;
  rel_path: string;
}): string {
  const stem = (t.rel_path.split("/").pop() ?? t.rel_path).replace(/\.[^.]+$/, "");
  const title = t.title?.trim() || stem;
  const artist = t.artist?.trim();
  return artist ? `${artist} — ${title}` : title;
}

/**
 * The confirm in front of every track delete (track menu, Delete key,
 * duplicates cleanup). Cancel has the focus, so Enter cancels too; only a
 * click on the red button deletes. Keys are caught before any other window
 * listener, so Escape or Delete never reaches the table or the modal behind.
 */
export default function DeleteConfirm({
  items,
  onConfirm,
  onCancel,
}: {
  items: DeleteItem[];
  onConfirm: () => void;
  onCancel: () => void;
}) {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      e.stopPropagation();
      if (e.key === "Escape") {
        e.preventDefault();
        onCancel();
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [onCancel]);

  const n = items.length;
  const permanent = items.filter((t) => t.onStick && !t.missing).length;
  const missing = items.filter((t) => t.missing).length;
  const binned = n - permanent - missing;

  return (
    <ModalBackdrop onClose={onCancel}>
      <div
        className="modal delete-confirm"
        role="alertdialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-title">
          Delete {n} track{n === 1 ? "" : "s"}?
        </div>
        <ul className="delete-list">
          {items.slice(0, SHOWN).map((t, i) => (
            <li key={i} className={t.onStick && !t.missing ? "permanent" : undefined}>
              {t.name}
            </li>
          ))}
          {n > SHOWN && <li className="more">+ {n - SHOWN} more</li>}
        </ul>
        {permanent > 0 && (
          <div className="delete-warn">
            {permanent === n ? (n === 1 ? "This file is" : "These files are") : `${permanent} of them are`}{" "}
            on a USB device and will be deleted <b>permanently</b>. USB devices have no Recycle Bin,
            so this cannot be undone.
          </div>
        )}
        {binned > 0 && (
          <div className="modal-note">
            {binned === n ? (n === 1 ? "The file goes" : "The files go") : `${binned} go`} to the
            Windows Recycle Bin, where you can restore {binned === 1 ? "it" : "them"}.
          </div>
        )}
        {missing > 0 && (
          <div className="modal-note">
            {missing === n ? (n === 1 ? "This file is" : "These files are") : `${missing} are`} already
            gone from disk; only {missing === 1 ? "its library entry is" : "their library entries are"}{" "}
            removed.
          </div>
        )}
        <div className="modal-actions">
          <button className="btn" autoFocus onClick={onCancel}>
            Cancel
          </button>
          <button className="btn danger" onClick={onConfirm}>
            {permanent > 0 ? "Delete permanently" : "Delete"}
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
