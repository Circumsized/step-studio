import { describe, expect, it } from "vitest";
import { agentStatusLabel, formatAgentDuration, formatAgentTokens } from "./App";

// The live Agents panel used to render `gen N · of <parent>` where the reference
// and the preview both show a metrics row. These three functions are what that
// row is made of, so they carry the behaviour the sign-off compares.
describe("formatAgentDuration", () => {
  it("shows seconds below a minute, padded to two digits above it", () => {
    expect(formatAgentDuration(0)).toBe("0s");
    expect(formatAgentDuration(38_400)).toBe("38s");
    // 59.999 s rounds to a whole 60 s, which then reads as a minute — the
    // boundary is asserted rather than avoided, so the rounding is on record.
    expect(formatAgentDuration(59_999)).toBe("1m 00s");
    expect(formatAgentDuration(60_000)).toBe("1m 00s");
    expect(formatAgentDuration(372_000)).toBe("6m 12s");
  });

  it("drops seconds past an hour, the way the reference cards do", () => {
    expect(formatAgentDuration(3_600_000)).toBe("1h 0m");
    expect(formatAgentDuration(7_920_000)).toBe("2h 12m");
  });

  it("treats a negative duration as zero rather than printing -1s", () => {
    expect(formatAgentDuration(-500)).toBe("0s");
  });
});

describe("formatAgentTokens", () => {
  it("keeps small counts exact and scales the rest to one decimal", () => {
    expect(formatAgentTokens(0)).toBe("0");
    expect(formatAgentTokens(999)).toBe("999");
    expect(formatAgentTokens(1000)).toBe("1.0k");
    expect(formatAgentTokens(48_200)).toBe("48.2k");
    expect(formatAgentTokens(1_234_567)).toBe("1234.6k");
  });
});

describe("agentStatusLabel", () => {
  const t = (key: string) => `zh:${key}`;

  it("translates every status the contract declares", () => {
    for (const status of ["starting", "running", "idle", "parked", "reviving", "aborted", "failed", "released"]) {
      expect(agentStatusLabel(status, t)).toBe(`zh:agent.status.${status}`);
    }
  });

  it("passes an unknown status through rather than showing a raw key or blank", () => {
    expect(agentStatusLabel("something-new", t)).toBe("something-new");
  });
});
