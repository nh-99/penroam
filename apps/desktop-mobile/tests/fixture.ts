import { readFileSync } from "node:fs";
import { resolve } from "node:path";
// Shared repo fixtures (tests run with cwd = apps/desktop-mobile).
export const fixture = (p: string) => readFileSync(resolve(process.cwd(), "../../tests/fixtures", p), "utf8");
