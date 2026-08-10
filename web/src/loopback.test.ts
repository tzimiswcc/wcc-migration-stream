import { describe, expect, it } from "vitest";
import { requireLoopbackOrigin } from "../browser/loopback";

describe("browser target configuration", () => {
  it.each([
    ["http://127.0.0.1:4173", "http://127.0.0.1:4173"],
    ["http://localhost:4173", "http://localhost:4173"],
    ["http://[::1]:4173", "http://[::1]:4173"],
  ])("accepts literal HTTP loopback origin %s", (value, expected) => {
    expect(requireLoopbackOrigin(value, "TEST_URL")).toBe(expected);
  });

  it.each([
    "https://127.0.0.1:4173",
    "http://example.test:4173",
    "http://127.0.0.1.example.test:4173",
    "http://user:secret@127.0.0.1:4173",
    "http://127.0.0.1:4173/path",
    "http://127.0.0.1:4173/?query=yes",
    "http://127.0.0.1:4173/#fragment",
    "not-a-url",
  ])("rejects a non-loopback browser configuration %s", (value) => {
    expect(() => requireLoopbackOrigin(value, "TEST_URL")).toThrowError(
      /literal HTTP loopback origin/,
    );
  });
});
