// Copies PDF.js runtime data (CMaps, standard fonts, wasm decoders, ICC
// profiles) into public/pdfjs so PDFs render fully offline. Run before dev/build.
import { cpSync, existsSync, mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pkg = dirname(createRequire(import.meta.url).resolve("pdfjs-dist/package.json"));
const out = join(here, "..", "public", "pdfjs");
mkdirSync(out, { recursive: true });
for (const d of ["cmaps", "standard_fonts", "wasm", "iccs"]) {
  if (existsSync(join(pkg, d))) cpSync(join(pkg, d), join(out, d), { recursive: true });
}
console.log(`pdf.js assets -> ${out}`);
