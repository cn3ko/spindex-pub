// Five stars for a 1-5 rating (null = unrated). With onChange the stars are
// buttons; clicking the current rating again clears it.
export default function StarRating({
  value,
  onChange,
}: {
  value: number | null;
  onChange?: (v: number | null) => void;
}) {
  return (
    <span className={`stars ${onChange ? "editable" : ""}`} aria-label={`${value ?? 0} of 5 stars`}>
      {[1, 2, 3, 4, 5].map((n) =>
        onChange ? (
          <button
            key={n}
            type="button"
            className={value != null && n <= value ? "on" : ""}
            title={value === n ? "Click again to clear" : `${n} star${n > 1 ? "s" : ""}`}
            onClick={() => onChange(value === n ? null : n)}
          >
            ★
          </button>
        ) : (
          <span key={n} className={value != null && n <= value ? "on" : ""}>
            ★
          </span>
        ),
      )}
    </span>
  );
}
