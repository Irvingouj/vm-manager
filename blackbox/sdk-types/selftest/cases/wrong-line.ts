// @selftest fail
// The error is one line below the one the expectation is about.
// @expect TS2322 is not assignable
export const fine: number = 1;
export const n: number = "not a number";
