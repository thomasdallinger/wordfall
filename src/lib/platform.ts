/**
 * Which OS the UI is running on, and the wording that depends on it.
 * The mock backend can preview Windows with `?platform=windows` (dev only).
 */
const override = import.meta.env.DEV ? new URLSearchParams(location.search).get('platform') : null;

export const IS_WINDOWS = override ? override === 'windows' : /Windows/i.test(navigator.userAgent);

/** "this Mac" / "this PC", for sentences like "runs on this Mac". */
export const THIS_DEVICE = IS_WINDOWS ? 'this PC' : 'this Mac';

/** Where the user grants permissions. */
export const SYSTEM_SETTINGS = IS_WINDOWS ? 'Windows Settings' : 'System Settings';

/** The primary shortcut modifier as shown in help text: "⌘" or "Ctrl+". */
export const MOD = IS_WINDOWS ? 'Ctrl+' : '⌘';

/** Whether the platform's primary shortcut modifier (⌘ or Ctrl) is held. */
export function primaryMod(e: KeyboardEvent): boolean {
  return IS_WINDOWS ? e.ctrlKey : e.metaKey;
}
