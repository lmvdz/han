import { fileURLToPath } from "node:url";
import { replayVectors } from "./vecreplay.js";
const r = await replayVectors(fileURLToPath(new URL("../../../conformance/vectors", import.meta.url)), process.argv[2]);
console.log(r.fail.join("\n"));
console.log(`vectors ${r.vectors}, replayable ${r.usable}, skipped ${r.skippedVectors.length}; steps ${r.steps}, pass ${r.pass}, fail ${r.fail.length}, post-move 'moved' ${r.movedAway}`);
process.exit(0);
