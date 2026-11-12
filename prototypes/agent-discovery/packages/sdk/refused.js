// The one error type for every operation the platform refuses.
export class Refused extends Error {
  constructor(refusal, fix) {
    super(`Refused (${refusal.reason}): ${fix}`);
    this.name = "Refused";
    this.refusal = refusal;
    this.fix = fix;
  }
}
