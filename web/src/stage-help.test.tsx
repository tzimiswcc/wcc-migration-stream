import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { type Journey, JOURNEY_STAGE_IDS } from "./contracts";
import { StageHelpPanel, stageHelpForLocation } from "./stage-help";
import { journeyFixture } from "./test/fixtures";

describe("stage help", () => {
  it("maps every stable journey stage to offline help", () => {
    const journey = journeyFixture as Journey;
    const routes = {
      SETUP: "/setup",
      DESTINATION: "/destination",
      SOURCE: "/files",
      MAPPING: "/mapping",
      VALIDATE: "/validation",
      COMPARE: "/comparison",
      ISSUES: "/issues",
      DECISIONS: "/decisions",
      RECONCILIATION: "/reconciliation",
      PACKAGE: "/publication",
    } as const;

    expect(Object.keys(routes)).toEqual([...JOURNEY_STAGE_IDS]);
    for (const stageId of JOURNEY_STAGE_IDS) {
      const location = new URL(routes[stageId], window.location.origin);
      expect(stageHelpForLocation(journey, location)).toBe(stageId);
    }
  });

  it("is keyboard-operable, stage-specific, and read-only", async () => {
    const user = userEvent.setup();
    const location = new URL("/mapping", window.location.origin);
    render(
      <StageHelpPanel
        journey={journeyFixture as Journey}
        location={location}
      />,
    );

    const region = screen.getByRole("region", { name: "Help for this stage" });
    expect(region).toHaveTextContent("Build the mapping contract");
    const button = screen.getByRole("button", { name: "Show help" });
    expect(button).toHaveAttribute("aria-expanded", "false");

    await user.tab();
    expect(button).toHaveFocus();
    await user.keyboard("{Enter}");

    expect(button).toHaveAttribute("aria-expanded", "true");
    expect(region).toHaveTextContent("value-free coverage");
    expect(region).toHaveTextContent("do not run arbitrary code");
    expect(region.querySelectorAll("a, input, select, textarea")).toHaveLength(
      0,
    );
  });

  it("uses the server current stage as a safe fallback for a supporting route", () => {
    const journey = {
      ...journeyFixture,
      currentStageId: "ISSUES",
    } as Journey;
    const location = new URL("/operations", window.location.origin);
    expect(stageHelpForLocation(journey, location)).toBe("ISSUES");
  });
});
