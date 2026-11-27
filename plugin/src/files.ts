import { chmodSync, existsSync, mkdirSync, readFileSync, renameSync, statSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

export type Plan = { path: string; before: string | null; after: string };

export const read = (path: string): string | null => (existsSync(path) ? readFileSync(path, "utf8") : null);

export const stamp = (): string => new Date().toISOString().replace(/\D/g, "").slice(0, 14);

export const write = (plan: Plan, when: string): void => {
  const dir = dirname(plan.path);
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true, mode: 0o700 });
  const mode = plan.before === null ? 0o600 : statSync(plan.path).mode & 0o777;
  if (plan.before !== null) writeFileSync(`${plan.path}.bak-localstudio-${when}`, plan.before, { mode });
  const tmp = `${plan.path}.localstudio-${process.pid}.tmp`;
  writeFileSync(tmp, plan.after, { mode });
  chmodSync(tmp, mode);
  renameSync(tmp, plan.path);
};
