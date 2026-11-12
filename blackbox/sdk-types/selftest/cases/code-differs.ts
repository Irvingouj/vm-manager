// @selftest fail
// The error is there with these words, but under another code.
// @expect TS2345 Type 'string' is not assignable to type 'number'.
export const n: number = "not a number";
