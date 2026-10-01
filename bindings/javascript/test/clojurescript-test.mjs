import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, realpathSync, rmSync, symlinkSync, unlinkSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const compilerOutput = resolve(packageRoot, "../../target/libdictenstein-cljs-r8");
const scopeDirectory = join(packageRoot, "node_modules", "@vinary-tree");
const selfLink = join(scopeDirectory, "libdictenstein");
let createdSelfLink = false;

function run(command, arguments_) {
  const result = spawnSync(command, arguments_, {
    cwd: packageRoot,
    encoding: "utf8",
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} ${arguments_.join(" ")} exited with ${result.status}`);
  }
}

mkdirSync(scopeDirectory, { recursive: true });
if (existsSync(selfLink)) {
  if (realpathSync(selfLink) !== realpathSync(packageRoot)) {
    throw new Error(`${selfLink} exists but does not resolve to the package under test`);
  }
} else {
  symlinkSync(packageRoot, selfLink, "dir");
  createdSelfLink = true;
}

try {
  run("clojure", ["-M:cljs-test"]);
  run(process.execPath, ["test/run-clojurescript.cjs"]);
} finally {
  if (createdSelfLink) unlinkSync(selfLink);
  rmSync(compilerOutput, { recursive: true, force: true });
}
