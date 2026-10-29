import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";

// `schema.ts` is generated from the server's `openapi.json` (spec 11), so the
// client's wire types can't drift from the server's. `npm run gen:api`
// regenerates it.
test("schema.ts matches openapi.json", () => {
  const here = (path: string) => fileURLToPath(new URL(path, import.meta.url));
  const fresh = execFileSync(here("../../node_modules/.bin/openapi-typescript"), [here("../../../openapi.json")], {
    encoding: "utf8",
  });
  expect(readFileSync(here("schema.ts"), "utf8"), "run `npm run gen:api`").toBe(fresh);
});
