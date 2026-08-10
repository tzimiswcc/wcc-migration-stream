import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SafeRenderBoundary } from "./ui";

function UnsafeRenderer(): never {
  throw new Error("sensitive renderer detail");
}

describe("safe render boundary", () => {
  afterEach(() => vi.restoreAllMocks());

  it("fails closed without exposing renderer diagnostics", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    render(
      <SafeRenderBoundary>
        <UnsafeRenderer />
      </SafeRenderBoundary>,
    );

    expect(screen.getByRole("alert")).toHaveTextContent("INVALID_RESPONSE");
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Evidence could not be displayed safely",
    );
    expect(screen.queryByText(/sensitive renderer detail/)).toBeNull();
  });
});
