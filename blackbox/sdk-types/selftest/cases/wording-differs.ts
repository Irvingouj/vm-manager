// @selftest fail
// The error is there, with the right code, but not with these words.
// @expect TS2322 Type 'string' is not assignable to type 'boolean'.
export const n: number = "not a number";
