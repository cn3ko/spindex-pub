import { useEffect, useState } from "react";
import {
  ACTIONS,
  comboOf,
  conflictWith,
  defaults,
  displayCombo,
  setBindings,
  useBindings,
  withBinding,
  type ActionId,
} from "../shortcuts";
import ModalBackdrop from "./ModalBackdrop";

const label = (id: ActionId) => ACTIONS.find((a) => a.id === id)?.label ?? id;

/**
 * Keyboard shortcut settings: every deck and grid-editor action with its key.
 * Click a key, then press the new one (Esc cancels). A key already in use
 * moves to the new action, and the notice says which one lost it.
 */
export default function ShortcutsDialog({ onClose }: { onClose: () => void }) {
  const keys = useBindings();
  const [capturing, setCapturing] = useState<ActionId | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // while capturing, this listener sees the key before anything else does
  // (the deck's play key, the editor's shortcuts) and keeps it to itself
  useEffect(() => {
    if (!capturing) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (e.key === "Escape") {
        setCapturing(null);
        return;
      }
      const combo = comboOf(e);
      if (!combo) return; // a modifier alone: keep waiting for the key
      const taken = conflictWith(keys, capturing, combo);
      setBindings(withBinding(keys, capturing, combo));
      setNotice(
        taken
          ? `${displayCombo(combo)} now does “${label(capturing)}”; “${label(taken)}” has no key now.`
          : null,
      );
      setCapturing(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [capturing, keys]);

  const groups = ["Deck", "Grid editor"] as const;
  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal shortcuts-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title">Keyboard shortcuts</div>
        <div className="modal-note">
          Click a key to change it, then press the new one (Esc cancels). Shift with
          a nudge key moves 10 ms instead of 1.
        </div>
        {notice && <div className="modal-note shortcuts-notice">{notice}</div>}
        {groups.map((g) => (
          <div className="shortcuts-group" key={g}>
            <div className="shortcuts-group-title">{g}</div>
            {ACTIONS.filter((a) => a.group === g).map((a) => (
              <div className="shortcuts-row" key={a.id}>
                <span className="shortcuts-label">{a.label}</span>
                <button
                  className={"shortcuts-key mono" + (capturing === a.id ? " capturing" : "")}
                  onClick={() => {
                    setNotice(null);
                    setCapturing(capturing === a.id ? null : a.id);
                  }}
                >
                  {capturing === a.id ? "Press a key…" : displayCombo(keys[a.id])}
                </button>
                <button
                  className="shortcuts-clear"
                  disabled={!keys[a.id]}
                  onClick={() => setBindings(withBinding(keys, a.id, null))}
                  data-tip="No key for this action"
                >
                  ✕
                </button>
              </div>
            ))}
          </div>
        ))}
        <div className="modal-actions">
          <button
            className="btn"
            onClick={() => {
              setBindings(defaults());
              setNotice(null);
              setCapturing(null);
            }}
          >
            Reset to defaults
          </button>
          <button className="btn primary" onClick={onClose}>
            Done
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
