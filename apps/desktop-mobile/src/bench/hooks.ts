// Handles the benchmark harness uses to reach the live editor (no UI).
import type { InkEditor } from "../editor/engine";
export const benchHooks: { editor: InkEditor | null } = { editor: null };
