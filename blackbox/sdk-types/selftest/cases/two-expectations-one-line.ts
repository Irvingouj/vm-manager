// @selftest pass
// Two errors on one line, each expected.
// @expect TS2322 Type 'string' is not assignable to type 'number'.
// @expect TS2322 Type 'boolean' is not assignable to type 'number'.
export const pair: [number, number] = ["one", true];
