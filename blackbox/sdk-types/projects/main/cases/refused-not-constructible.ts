// Application code cannot make a Refused: the constructor is private
// (CONTRACT.md, "The types"; at run time it throws a TypeError, which
// blackbox/tests/sdk_refusals.rs checks).
import { Refused } from "@vm/sdk";

// @expect TS2673 Constructor of class 'Refused' is private
export const bare = new Refused();
// @expect TS2673 Constructor of class 'Refused' is private
export const withArguments = new Refused({ reason: "no-grant" }, "Ask for access.");
