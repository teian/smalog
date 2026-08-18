/** Paging state for the two System views.
 *
 *  Both walk the same ring in two directions: `since` pulls new entries onto
 *  the head while the view follows the tail, `before` appends older pages when
 *  the operator reaches the end of what is loaded. The retained window is up
 *  to two days, so it is never held in the browser all at once.
 */

/** Anything the ring returns: identified by a monotonic cursor. */
export interface RingEntry {
  sequence: number;
}

export interface RingWindow {
  retentionHours: number;
  maxEntries: number;
  retained: number;
  oldestOccurredAt: number | null;
  dropped: number;
  /** The service restarted its ring under us; the page begins again at the
   *  newest record rather than continuing from the cursor we sent. */
  reset?: boolean;
}

export interface RingPage<T extends RingEntry> {
  entries: T[];
  envelope: RingWindow;
}

export interface RingState<T extends RingEntry> {
  entries: T[];
  window: RingWindow | null;
  /** No older page remains: the oldest retained entry is loaded. */
  atOldest: boolean;
}

export const emptyRingState = <T extends RingEntry>(): RingState<T> => ({
  entries: [],
  window: null,
  atOldest: false,
});

/** Newest cursor held, for the next `since` request. */
export function newestCursor<T extends RingEntry>(state: RingState<T>): number | null {
  return state.entries.length > 0 ? state.entries[0].sequence : null;
}

/** Oldest cursor held, for the next `before` request. */
export function oldestCursor<T extends RingEntry>(state: RingState<T>): number | null {
  return state.entries.length > 0
    ? state.entries[state.entries.length - 1].sequence
    : null;
}

function sortNewestFirst<T extends RingEntry>(entries: T[]): T[] {
  return [...entries].sort((a, b) => b.sequence - a.sequence);
}

function deduplicate<T extends RingEntry>(entries: T[]): T[] {
  const seen = new Set<number>();
  return entries.filter((entry) => {
    if (seen.has(entry.sequence)) return false;
    seen.add(entry.sequence);
    return true;
  });
}

/** Merge a `since` page onto the head, keeping at most `keep` entries.
 *
 *  Trimming the tail is what keeps a following view bounded; the entries
 *  dropped here are still in the database and come back through `before`.
 */
export function mergeNewer<T extends RingEntry>(
  state: RingState<T>,
  page: RingPage<T>,
  keep: number,
): RingState<T> {
  // A reset means our cursors belong to a ring that no longer exists — the
  // service restarted. Keeping the old entries would interleave two
  // unrelated sequences, so start over from what the service has now.
  if (page.envelope.reset) return replaceAll(page);
  const merged = deduplicate(sortNewestFirst([...page.entries, ...state.entries]));
  const trimmed = merged.slice(0, Math.max(keep, 1));
  return {
    entries: trimmed,
    window: page.envelope,
    // Trimming the tail means older entries are reachable again.
    atOldest: state.atOldest && trimmed.length === merged.length,
  };
}

/** Append a `before` page to the tail. An empty page means the oldest
 *  retained entry has been reached. */
export function appendOlder<T extends RingEntry>(
  state: RingState<T>,
  page: RingPage<T>,
): RingState<T> {
  if (page.entries.length === 0) {
    return { ...state, window: page.envelope, atOldest: true };
  }
  return {
    entries: deduplicate(sortNewestFirst([...state.entries, ...page.entries])),
    window: page.envelope,
    atOldest: false,
  };
}

/** Replace everything, for a filter change. */
export function replaceAll<T extends RingEntry>(page: RingPage<T>): RingState<T> {
  return {
    entries: deduplicate(sortNewestFirst(page.entries)),
    window: page.envelope,
    atOldest: page.entries.length === 0,
  };
}

export type RingWindowStatus =
  | { kind: "disabled" }
  | { kind: "empty"; retentionHours: number }
  | { kind: "full"; retentionHours: number; oldestOccurredAt: number }
  | {
      /** The row cap cut the window short — say so rather than claim 48 h. */
      kind: "shortened";
      retentionHours: number;
      actualHours: number;
      oldestOccurredAt: number;
    };

/** What the view should say about the window it is showing.
 *
 *  `now` is passed in so the result is a pure function of its inputs.
 */
export function windowStatus(
  window: RingWindow | null,
  now: number,
): RingWindowStatus {
  if (!window || window.retentionHours === 0) return { kind: "disabled" };
  if (window.retained === 0 || window.oldestOccurredAt == null) {
    return { kind: "empty", retentionHours: window.retentionHours };
  }
  const actualHours = (now - window.oldestOccurredAt) / 3_600_000;
  // Only call a window shortened when it is meaningfully short: a ring that
  // simply has not been running for two days yet is not a cap problem, but
  // the view cannot tell those apart, so it reports the actual window either
  // way and only flags the case where the cap is provably the cause.
  if (window.retained >= window.maxEntries && actualHours < window.retentionHours) {
    return {
      kind: "shortened",
      retentionHours: window.retentionHours,
      actualHours,
      oldestOccurredAt: window.oldestOccurredAt,
    };
  }
  return {
    kind: "full",
    retentionHours: window.retentionHours,
    oldestOccurredAt: window.oldestOccurredAt,
  };
}
