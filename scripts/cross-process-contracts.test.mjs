// Cross-process environment-variable contracts.
//
// Some variables are not configuration — they are the wiring between two processes
// that are built and released independently. The host sets them; the Runtime (the
// `step` binary, whose source lives in omp-patch/ and is compiled separately) reads
// them. If either side renames its half, nothing fails at build time and nothing
// fails loudly at run time: the feature just silently stops working.
//
// That has already happened once. `ea787b9` renamed the host side's `OMP_*` family to
// `STEP_STUDIO_*` for consistency, and took `OMP_STATS_NO_OPEN` with it — the one
// variable whose own comment says it exists so that "a host written against an older
// Runtime ... keeps working". The stats dashboard started opening a browser again.
//
// So the contract is written down here, and this suite asserts both halves still
// name it. Adding a contract means adding a row; changing one means changing both
// sides in the same commit.
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/**
 * Every variable that crosses the host/Runtime process boundary.
 *
 * `setters` and `readers` are directories searched recursively for the literal
 * name; a contract is intact when both sides mention it. `why` is what breaks when
 * it is not — written so the next person does not have to reconstruct it.
 */
const CONTRACTS = [
  {
    name: "OMP_STUDIO_PARENT_PID",
    readers: ["omp-patch/overlay"],
    wired: true,
    why: "the Runtime's parent watchdog never learns the Host's pid, so a crashed or " +
         "force-quit Host leaves the Runtime running; on macOS the process-group " +
         "fallback only kills direct children, so tools and subagents leak",
  },
  {
    name: "OMP_STATS_NO_OPEN",
    readers: ["omp-patch/overlay", "omp-patch/vendor/oh-my-pi/packages/coding-agent/src/cli"],
    wired: true,
    why: "`omp stats` decides it has no embedding host and opens the system browser " +
         "on every visit, duplicating the in-app dashboard",
  },
  // The three below were unwired when this suite was written: the Runtime read them
  // and nothing in this repository set them, which was issue #5. They are wired now —
  // `studio-host`'s `bring_up` prepares a `0700` root under the state directory for
  // each and exports the path. They stay in the table because a rename on either side
  // is still a silent regression, and because the next person to "tidy" the env names
  // should have to edit this file to do it.
  {
    name: "OMP_STUDIO_MEDIA_ROOT",
    readers: ["omp-patch/overlay"],
    wired: true,
    why: "the media service reports available: false and the generation queue stays " +
         "empty, with no reason shown to the user",
  },
  {
    name: "OMP_STUDIO_SOCKET_DIR",
    readers: ["omp-patch/overlay"],
    wired: true,
    why: "live audio and prediction lose their socket directory and degrade to " +
         "no persistence",
  },
  {
    name: "OMP_STUDIO_AUTH_BROKER_FILE",
    readers: ["omp-patch/overlay"],
    wired: true,
    why: "the auth broker closes on a missing file and ships an empty implementation",
  },
];

const SKIP_DIRS = new Set(["node_modules", "dist", "target", ".git", "__shots__"]);

/** Every file under `dirs`, recursively, skipping build output. */
function filesUnder(dirs) {
  const out = [];
  const walk = (dir) => {
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (SKIP_DIRS.has(entry.name)) continue;
      const full = join(dir, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (entry.isFile()) out.push(full);
    }
  };
  for (const dir of dirs) walk(join(root, dir));
  return out;
}

function mentions(files, needle) {
  return files.some((file) => {
    let text;
    try {
      text = readFileSync(file, "utf8");
    } catch {
      return false;
    }
    return text.includes(needle);
  });
}

const HOST_FILES = filesUnder(["packages", "crates", "apps"]);

for (const contract of CONTRACTS) {
  test(`${contract.name} is named the same on both sides of the process boundary`, () => {
    const readerFiles = filesUnder(contract.readers);

    assert.ok(
      mentions(readerFiles, contract.name),
      `nothing in ${contract.readers.join(", ")} reads ${contract.name}. ` +
        `Consequence: ${contract.why}.`,
    );

    if (!contract.wired) return;

    const setterFiles = filesUnder(["packages", "crates", "apps"]);
    assert.ok(
      mentions(setterFiles, contract.name),
      `nothing in packages, crates or apps sets ${contract.name}. ` +
        `Consequence: ${contract.why}. If the Runtime side was renamed, rename this ` +
        `side in the same commit; if the contract was retired, delete this row.`,
    );
  });
}

test("the host side does not carry a renamed copy of a contract", () => {
  // The failure mode this suite exists for: the host side gets a tidier name and the
  // Runtime keeps the old one. Catch the near-miss directly — any host-side variable
  // that differs from a contract only by the OMP_/STEP_STUDIO_ prefix is a suspect.
  const suspects = [];
  for (const file of HOST_FILES) {
    let text;
    try {
      text = readFileSync(file, "utf8");
    } catch {
      continue;
    }
    for (const contract of CONTRACTS) {
      for (const prefix of ["STEP_STUDIO_", "STEP_"]) {
        const tidied = contract.name.replace(/^OMP_/, prefix);
        if (tidied !== contract.name && text.includes(tidied)) {
          suspects.push(`${file.replace(`${root}/`, "")}: ${tidied}`);
        }
      }
    }
  }
  assert.deepEqual(suspects, [], `renamed copies of a contract found: ${suspects.join(", ")}`);
});
