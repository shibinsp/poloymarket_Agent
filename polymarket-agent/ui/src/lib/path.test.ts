import { describe, expect, it } from "vitest";
import { splitSegments } from "./path";

describe("splitSegments", () => {
  it("keeps contiguous points together and splits at missing readings", () => {
    expect(
      splitSegments([
        [0, 1],
        [1, 2],
        null,
        [3, 4],
        null,
        [5, 6],
      ]),
    ).toEqual([
      [
        [0, 1],
        [1, 2],
      ],
      [[3, 4]],
      [[5, 6]],
    ]);
  });

  it("returns no segments for only missing readings", () => {
    expect(splitSegments([null, null])).toEqual([]);
  });
});
