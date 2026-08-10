import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";
import "../styles.css";

Object.defineProperty(window, "scrollTo", {
  configurable: true,
  value: vi.fn(),
});

Object.defineProperty(HTMLCanvasElement.prototype, "getContext", {
  configurable: true,
  value: vi.fn(() => null),
});

afterEach(() => {
  cleanup();
  window.history.replaceState(null, "", "/");
});
