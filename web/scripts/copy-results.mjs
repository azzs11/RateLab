// Copies the benchmark's result files into public/data so the site shows the same
// numbers as results/RESULTS.md. Missing files are skipped; the page says so.
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const from = join(here, "..", "..", "results");
const to = join(here, "..", "public", "data");
mkdirSync(to, { recursive: true });
for (const name of ["local.json", "memory.json", "cluster.json", "strategies.json", "traces.json", "real_strategies.json", "overload.json"]) {
  const src = join(from, name);
  if (existsSync(src)) {
    copyFileSync(src, join(to, name));
    console.log(`copied ${name}`);
  } else {
    console.log(`skipped ${name} (not measured yet)`);
  }
}
