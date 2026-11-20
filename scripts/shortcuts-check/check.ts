// Assertions for src/shortcuts.ts (the pure part). Run from the repo root:
//   npx esbuild scripts/shortcuts-check/check.ts --bundle --platform=node \
//     --outfile=scripts/shortcuts-check/check.mjs --format=esm && node scripts/shortcuts-check/check.mjs
import {
  actionFor, ACTIONS, comboOf, conflictWith, defaults, displayCombo, merge, withBinding,
} from "../../src/shortcuts";

let failed = 0;
function ok(cond: boolean, msg: string) {
  if (!cond) {
    failed++;
    console.error("FAIL:", msg);
  }
}
const ev = (key: string, mods: Partial<{ ctrl: boolean; shift: boolean; alt: boolean; meta: boolean }> = {}) => ({
  key,
  ctrlKey: !!mods.ctrl,
  shiftKey: !!mods.shift,
  altKey: !!mods.alt,
  metaKey: !!mods.meta,
});

// naming a key press
ok(comboOf(ev("l")) === "L", "letters are upper case");
ok(comboOf(ev("L", { shift: true })) === "Shift+L", "Shift counts on letters");
ok(comboOf(ev("s", { ctrl: true })) === "Ctrl+S", "Ctrl+S");
ok(comboOf(ev("s", { meta: true })) === "Ctrl+S", "Cmd counts as Ctrl");
ok(comboOf(ev(" ")) === "Space", "space bar");
ok(comboOf(ev("+", { shift: true })) === "+", "a shifted symbol is its own key: + on a US layout");
ok(comboOf(ev("?", { shift: true })) === "?", "? whatever the layout needs to type it");
ok(comboOf(ev("ArrowLeft", { shift: true })) === "Shift+ArrowLeft", "named keys keep Shift");
ok(comboOf(ev("Shift", { shift: true })) === null, "a modifier alone is no shortcut");
ok(comboOf(ev("Dead")) === null, "a dead key is no shortcut");
ok(displayCombo("Shift+ArrowLeft") === "Shift+←", "arrows display as arrows");
ok(displayCombo(null) === "—", "unbound shows a dash");

// defaults: every action has one, and no two share a key
{
  const d = defaults();
  const keys = ACTIONS.map((a) => d[a.id]).filter((k): k is string => !!k);
  ok(keys.length === ACTIONS.length, "every action has a default key");
  ok(new Set(keys).size === keys.length, "no default key is used twice");
}

// which action a press means
{
  const d = defaults();
  ok(actionFor("I", d)?.id === "loopIn", "I marks In");
  ok(actionFor("Shift+L", d)?.id === "loopClear", "Shift+L clears, distinct from L");
  const fine = actionFor("Shift+ArrowLeft", d);
  ok(fine?.id === "nudgeEarlier" && fine.fine === true, "Shift on a nudge key is the 10 ms step");
  ok(actionFor("Q", d) === null, "an unbound key does nothing");
}

// rebinding: a key taken elsewhere moves, so no two actions ever share it
{
  const d = defaults();
  ok(conflictWith(d, "tap", "I") === "loopIn", "I is taken by loop In");
  ok(conflictWith(d, "loopIn", "I") === null, "an action does not conflict with itself");
  const b = withBinding(d, "tap", "I");
  ok(b.tap === "I" && b.loopIn === null, "the other action is unbound, not left sharing it");
  ok(d.loopIn === "I", "the input is not mutated");
}

// stored settings: only valid entries survive
{
  const m = merge({ tap: "Y", loopIn: null, nonsense: "X", click: 5 });
  ok(m.tap === "Y", "a stored key is used");
  ok(m.loopIn === null, "a stored unbinding is kept");
  ok(m.click === defaults().click, "a broken entry falls back to the default");
  ok(!("nonsense" in m), "unknown actions are dropped");
  ok(merge("garbage").tap === defaults().tap, "unreadable storage means defaults");
  const clash = merge({ tap: "I" });
  ok(clash.tap === "I" && clash.loopIn === null, "a stored key wins over a default it collides with");
}

if (failed) {
  console.error(`${failed} shortcuts check(s) failed`);
  process.exit(1);
}
console.log("SHORTCUTS-OK");
