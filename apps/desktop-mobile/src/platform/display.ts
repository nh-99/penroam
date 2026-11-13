// Making text from other people safe to show.
//
// Names (verified emails, display names) and comment text come from other accounts. Invisible
// bidirectional controls in them can reorder the text around them, so a name could make "Remove
// Eve" read as something else, and an internationalized look-alike domain can pass for a real one.
// `cleanName` removes those controls; templates also wrap names in `<bdi>` so that what remains
// can't reorder the surrounding text either. `linkHost` gives the host as the browser will use it
// (punycode), so a look-alike is visible. All pure.

/** Embeddings, overrides, isolates, directional marks and the Arabic letter mark. */
const BIDI_ALL = /[‪-‮⁦-⁩‎‏؜]/g;
/** The ones that force an order on what follows; marks (LRM/RLM/ALM) are left in running text. */
const BIDI_FORCING = /[‪-‮⁦-⁩]/g;

/** A name without bidi control characters. */
export const cleanName = (s: string | undefined | null): string => (s ?? "").replace(BIDI_ALL, "");

/** Running text (a comment) without the controls that force a direction. */
export const cleanText = (s: string): string => s.replace(BIDI_FORCING, "");

/**
 * The host a link goes to, as parsed (`new URL(u).host`: punycode for international names, with
 * any port), or null when it isn't a URL.
 */
export function linkHost(url: string): string | null {
  try {
    return new URL(url).host;
  } catch {
    return null;
  }
}

/** The part of the link text between `//` and the path: host, and possibly `user@` in front. */
const writtenAuthority = (text: string) => /^[a-z][a-z0-9+.-]*:\/\/([^/?#]*)/i.exec(text)?.[1]?.toLowerCase() ?? null;

/**
 * The host to show next to a link when it differs from what the link text says (or the text has a `user@` in front): an
 * internationalized look-alike (`аpple.com` with a Cyrillic а) shows as `xn--…`. Null when the
 * text already shows the real host.
 */
export function revealedHost(text: string, href: string): string | null {
  const real = linkHost(href);
  if (!real) return null;
  // `https://apple.com@evil.example/` reads like apple.com but goes elsewhere: the authority then
  // differs from the host, and the host is shown
  return writtenAuthority(text) === real.toLowerCase() ? null : real;
}

/** Keys whose string values are names of people. */
const NAME_KEYS = new Set(["display", "ownerDisplay", "invitedBy", "memberDisplay", "authorDisplay", "actorDisplay", "resolvedByDisplay", "email"]);

/** Cleans every name field in a command result or event payload, however deeply nested. */
export function cleanNames<T>(value: T): T {
  if (Array.isArray(value)) return value.map((v) => cleanNames(v)) as unknown as T;
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) out[k] = NAME_KEYS.has(k) && typeof v === "string" ? cleanName(v) : cleanNames(v);
    return out as T;
  }
  return value;
}
