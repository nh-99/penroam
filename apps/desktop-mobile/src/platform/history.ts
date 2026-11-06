// Version history for display. Every pen stroke is a version, far too many to list, so
// versions close together become one writing session, shown as its final state.
import type { HistoryRow } from "./ipc";

/** A pause this long (ms) starts a new session. */
const SESSION_GAP = 2 * 60 * 1000;

export interface HistorySession {
  /** The version at the end of the session, on this device. */
  revision: number | null;
  /** The same, in the account's operation log. */
  position: number | null;
  at: number;
  added: number;
  removed: number;
}

/** Whether a session came from the account rather than this device's own record. */
export const fromAccount = (s: HistorySession): boolean => s.revision === null;

/**
 * Groups versions (oldest first) into sessions, oldest first. The very first version stands
 * alone: it's the state before any recorded change, the one thing a session's end can't show.
 * A session never spans the two sources, because a version is restored from one or the other.
 */
export function groupSessions(points: HistoryRow[]): HistorySession[] {
  const out: HistorySession[] = [];
  let lastAt = -Infinity;
  for (const p of points) {
    const s = out[out.length - 1];
    if (s && out.length > 1 && p.at - lastAt < SESSION_GAP && (s.revision === null) === (p.revision === null)) {
      s.revision = p.revision;
      s.position = p.position;
      s.at = p.at;
      s.added += p.added;
      s.removed += p.removed;
    } else {
      out.push({ revision: p.revision, position: p.position, at: p.at, added: p.added, removed: p.removed });
    }
    lastAt = p.at;
  }
  return out;
}
