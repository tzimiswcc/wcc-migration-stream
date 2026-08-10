import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { AppLink, useLocation } from "./router";

function LocationProbe() {
  const location = useLocation();
  return (
    <>
      <AppLink href="/setup#mapping">Open mapping</AppLink>
      <output aria-label="Current location">
        {location.pathname}
        {location.search}
        {location.hash}
      </output>
    </>
  );
}

describe("hash-aware routing", () => {
  beforeEach(() =>
    window.history.replaceState(null, "", "/setup#profiled-source"),
  );

  it("retains fragments during navigation and publishes native hash changes", async () => {
    const user = userEvent.setup();
    render(<LocationProbe />);

    expect(screen.getByLabelText("Current location")).toHaveTextContent(
      "/setup#profiled-source",
    );

    await user.click(screen.getByRole("link", { name: "Open mapping" }));
    expect(window.location.pathname).toBe("/setup");
    expect(window.location.hash).toBe("#mapping");
    expect(screen.getByLabelText("Current location")).toHaveTextContent(
      "/setup#mapping",
    );

    act(() => {
      window.history.replaceState(null, "", "/setup#validation");
      window.dispatchEvent(new HashChangeEvent("hashchange"));
    });
    await waitFor(() =>
      expect(screen.getByLabelText("Current location")).toHaveTextContent(
        "/setup#validation",
      ),
    );
  });
});
