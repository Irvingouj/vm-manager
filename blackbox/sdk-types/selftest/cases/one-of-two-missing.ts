// @selftest fail
// Two errors expected on a line that has one.
// @expect TS2322 Type 'string' is not assignable to type 'number'.
// @expect TS2322 Type 'boolean' is not assignable to type 'number'.
export const n: number = "not a number";
