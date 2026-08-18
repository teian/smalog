import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  appendOlder,
  emptyRingState,
  mergeNewer,
  newestCursor,
  oldestCursor,
  replaceAll,
  windowStatus,
  type RingPage,
  type RingWindow,
} from "../src/lib/systemLog.ts";

interface Entry {
  sequence: number;
  label: string;
}

const HOUR_MS = 3_600_000;
const NOW = 1_800_000_000_000;

function window(overrides: Partial<RingWindow> = {}): RingWindow {
  return {
    retentionHours: 48,
    maxEntries: 50_000,
    retained: 10,
    oldestOccurredAt: NOW - 40 * HOUR_MS,
    dropped: 0,
    ...overrides,
  };
}

function page(sequences: number[], overrides: Partial<RingWindow> = {}): RingPage<Entry> {
  return {
    entries: sequences.map((sequence) => ({ sequence, label: `#${sequence}` })),
    envelope: window(overrides),
  };
}

describe("System view ring paging", () => {
  it("starts empty and reports no cursors", () => {
    const state = emptyRingState<Entry>();

    assert.deepEqual(state.entries, []);
    assert.equal(newestCursor(state), null);
    assert.equal(oldestCursor(state), null);
    assert.equal(state.window, null);
  });

  it("keeps entries newest first when a since page arrives", () => {
    const state = replaceAll(page([5, 4, 3]));
    const merged = mergeNewer(state, page([7, 6]), 100);

    assert.deepEqual(
      merged.entries.map((entry) => entry.sequence),
      [7, 6, 5, 4, 3],
    );
    assert.equal(newestCursor(merged), 7);
    assert.equal(oldestCursor(merged), 3);
  });

  it("deduplicates an entry delivered by both directions", () => {
    const state = replaceAll(page([5, 4]));
    const merged = mergeNewer(state, page([6, 5]), 100);

    assert.deepEqual(
      merged.entries.map((entry) => entry.sequence),
      [6, 5, 4],
    );
  });

  it("trims a following view to its cap and reopens older paging", () => {
    const state = { ...replaceAll(page([3, 2, 1])), atOldest: true };
    const merged = mergeNewer(state, page([5, 4]), 3);

    assert.deepEqual(
      merged.entries.map((entry) => entry.sequence),
      [5, 4, 3],
    );
    assert.equal(
      merged.atOldest,
      false,
      "entries trimmed off the tail are reachable again",
    );
  });

  it("appends an older page to the tail", () => {
    const state = replaceAll(page([9, 8]));
    const older = appendOlder(state, page([7, 6]));

    assert.deepEqual(
      older.entries.map((entry) => entry.sequence),
      [9, 8, 7, 6],
    );
    assert.equal(older.atOldest, false);
    assert.equal(oldestCursor(older), 6);
  });

  it("marks the end of the retained window on an empty older page", () => {
    const state = replaceAll(page([9, 8]));
    const older = appendOlder(state, { entries: [], envelope: window() });

    assert.deepEqual(
      older.entries.map((entry) => entry.sequence),
      [9, 8],
      "reaching the end must not discard what is loaded",
    );
    assert.equal(older.atOldest, true);
  });

  it("starts over when the service reset its ring", () => {
    const state = replaceAll(page([9, 8, 7]));
    // The log ring lives in the service's memory and restarts its sequence
    // with the process; keeping the old entries would interleave two
    // unrelated sequences in one list.
    const merged = mergeNewer(state, page([2, 1], { reset: true }), 100);

    assert.deepEqual(
      merged.entries.map((entry) => entry.sequence),
      [2, 1],
    );
  });

  it("treats an empty replacement as already at the oldest entry", () => {
    const state = replaceAll(page([]));

    assert.deepEqual(state.entries, []);
    assert.equal(state.atOldest, true);
  });
});

describe("System view window status", () => {
  it("reports a disabled ring", () => {
    assert.deepEqual(windowStatus(null, NOW), { kind: "disabled" });
    assert.deepEqual(windowStatus(window({ retentionHours: 0 }), NOW), {
      kind: "disabled",
    });
  });

  it("reports an empty ring separately from a disabled one", () => {
    assert.deepEqual(
      windowStatus(window({ retained: 0, oldestOccurredAt: null }), NOW),
      { kind: "empty", retentionHours: 48 },
    );
  });

  it("reports the configured window when the cap is not reached", () => {
    const status = windowStatus(window({ retained: 1_000 }), NOW);

    assert.equal(status.kind, "full");
  });

  it("reports a window the row cap cut short", () => {
    const status = windowStatus(
      window({ retained: 50_000, oldestOccurredAt: NOW - 9 * HOUR_MS }),
      NOW,
    );

    assert.equal(status.kind, "shortened");
    assert.equal(
      status.kind === "shortened" ? Math.round(status.actualHours) : 0,
      9,
      "an operator must not be told 48 h when only 9 h are retained",
    );
  });

  it("does not call a full-length window shortened at the cap", () => {
    const status = windowStatus(
      window({ retained: 50_000, oldestOccurredAt: NOW - 48 * HOUR_MS }),
      NOW,
    );

    assert.equal(status.kind, "full");
  });
});
