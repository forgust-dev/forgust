/** Apple keyboards: ⌘ where others press Ctrl. */
export const IS_MAC = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
/** The modifier's name in a key hint. */
export const MOD = IS_MAC ? "⌘" : "Ctrl";

/**
 * True when a key press belongs to an input method composing a word (Japanese, Chinese,
 * Korean…): its Enter takes a candidate, its arrows move among them, its Escape drops them.
 * A field's own keys (pick a suggestion, go to a result) wait until it is done. Safari has
 * already ended the composition when the Enter that confirms it arrives: keyCode 229 tells.
 */
export function composing(e: KeyboardEvent | { nativeEvent: KeyboardEvent }): boolean {
  const k = "nativeEvent" in e ? e.nativeEvent : e;
  return k.isComposing || k.keyCode === 229;
}
