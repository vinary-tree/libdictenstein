#!/usr/bin/env node

import { runCollectionTraversalProfile } from "../benchmarks/collection-traversal-profile.mjs";

function argumentsFrom(commandLine) {
  const result = { runtime: "native" };
  for (let index = 0; index < commandLine.length; index += 2) {
    const option = commandLine[index];
    const value = commandLine[index + 1];
    if (value === undefined) throw new Error(`missing value for ${option}`);
    switch (option) {
      case "--runtime": result.runtime = value; break;
      case "--arm": result.arm = value; break;
      case "--entries": result.entries = Number(value); break;
      case "--passes": result.passes = Number(value); break;
      case "--warmup-passes": result.warmupPasses = Number(value); break;
      case "--batch-size": result.batchSize = Number(value); break;
      case "--early-cancel": result.earlyCancel = Number(value); break;
      default: throw new Error(`unknown argument: ${option}`);
    }
  }
  if (!["native", "browser", "wasi"].includes(result.runtime)) {
    throw new Error("--runtime must be native, browser, or wasi");
  }
  return result;
}

try {
  const config = argumentsFrom(process.argv.slice(2));
  const path = config.runtime === "native" ? "../facades/native.mjs"
    : config.runtime === "browser" ? "../facades/wasm.mjs" : "../facades/wasi.mjs";
  const namespace = (await import(path)).default;
  console.log(JSON.stringify(runCollectionTraversalProfile(namespace, {
    ...config,
    runtime: `javascript-${config.runtime}`,
  })));
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 2;
}
