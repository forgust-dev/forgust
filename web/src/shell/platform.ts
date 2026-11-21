/** Apple keyboards: ⌘ where others press Ctrl. */
export const IS_MAC = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
/** The modifier's name in a key hint. */
export const MOD = IS_MAC ? "⌘" : "Ctrl";
