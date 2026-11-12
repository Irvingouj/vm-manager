#!/usr/bin/env node
import { main } from "../lib/commands.mjs";

main(process.argv.slice(2)).then(
  (code) => {
    if (typeof code === "number") process.exitCode = code;
  },
  (e) => {
    console.error(e?.stack ?? String(e));
    process.exitCode = 1;
  },
);
