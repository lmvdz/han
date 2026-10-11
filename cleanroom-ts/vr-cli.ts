import { replayVectors } from "./vecreplay.js";
const r = await replayVectors("H:/HAN/v1/ref/vectors", process.argv[2]);
console.log(r.fail.join("\n"));
console.log(`vectors ${r.vectors}, replayable ${r.usable}, skipped ${r.skippedVectors.length}; steps ${r.steps}, pass ${r.pass}, fail ${r.fail.length}, post-move 'moved' ${r.movedAway}`);
process.exit(0);
