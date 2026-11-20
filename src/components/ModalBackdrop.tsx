import { useRef, type ReactNode } from "react";

/**
 * Full-screen modal backdrop that dismisses on a genuine backdrop click only.
 *
 * A plain `onClick={onClose}` on the backdrop has a subtle bug: selecting text
 * with the mouse starting inside an input and releasing on the backdrop fires a
 * `click` whose target is the backdrop (the common ancestor), closing the modal
 * and losing edits. We close only when the press BOTH started and ended on the
 * backdrop itself, so a text-selection drag that merely ends there never closes.
 */
export default function ModalBackdrop({
  onClose,
  children,
}: {
  onClose: () => void;
  children: ReactNode;
}) {
  const pressedOnBackdrop = useRef(false);
  return (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        pressedOnBackdrop.current = e.target === e.currentTarget;
      }}
      onClick={(e) => {
        if (pressedOnBackdrop.current && e.target === e.currentTarget) onClose();
      }}
    >
      {children}
    </div>
  );
}
