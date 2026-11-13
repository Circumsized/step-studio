/**
 * The transport contract suite, invoked for the one adapter that ships.
 *
 * It used to run twice — once for the desktop adapter and once for the web one —
 * so both executed byte-for-byte the same checks. The web adapter's only consumer
 * was this test: no product in this workspace serves a browser, the plan scopes the
 * shell to Windows, and `transport-web` was deleted with the rest of M7. What is
 * left is the desktop adapter's contract, which is the one the Tauri shell drives.
 *
 * The call site below is still a compile-time pin: the adapter's API type must
 * accept the shared `ContractFixtureApi` shape.
 */

import { createDesktopTransport } from "@omp-studio/transport-desktop";

import { runTransportContract } from "../src/index.js";

runTransportContract("desktop", (api) => createDesktopTransport(api));
