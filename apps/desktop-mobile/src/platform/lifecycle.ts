// App-level flush hooks: the window close handler and app backgrounding call
// every registered flusher before the process may exit.
type Flusher = () => Promise<void>;
const flushers = new Set<Flusher>();

export function registerFlusher(f: Flusher): () => void {
  flushers.add(f);
  return () => flushers.delete(f);
}

export async function flushEverything(): Promise<void> {
  const errors: unknown[] = [];
  for (const f of flushers) {
    try {
      await f();
    } catch (e) {
      errors.push(e);
    }
  }
  if (errors.length) throw errors[0];
}
