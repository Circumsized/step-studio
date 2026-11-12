// §14.6 names one render-side hot path: markdown incremental parse frame time.
// It is the one that runs once per delta, so it is the one a long reply pays for
// every frame.
//
// Headless, no framework: the scanner is a pure function, so this drives it the
// way the renderer does — feed the document one chunk at a time, handing the
// previous frame's result back — and reports the frame-time distribution §14.6
// asks for (p50 / p99 / p99.9, plus the drop-frame count).
import { scanStreamingMarkdown } from "../../apps/renderer/src/conversation/markdownBlocks.ts";

/** A reply shaped like the expensive case the module's own comment names: a
 *  long streaming fenced code block, plus prose and a table around it. */
function document(chunks) {
  const prose = Array.from({ length: 40 }, (_, index) =>
    `Paragraph ${index} with **bold**, _italic_, \`code\`, and a [link](https://example.com/${index}).`,
  ).join("\n\n");
  const fence = ["```ts", ...Array.from({ length: 400 }, (_, index) =>
    `const value${index} = compute(${index}) + ${index * 7}; // a line of streaming code`), "```"].join("\n");
  const table = [
    "| column | value | note |",
    "| --- | --- | --- |",
    ...Array.from({ length: 60 }, (_, index) => `| row ${index} | ${index * 13} | note ${index} |`),
  ].join("\n");
  return [prose, fence, table, prose].join("\n\n");
}

/** Split into chunks the way deltas arrive: small, uneven, and append-only. */
function chunksOf(text, target) {
  const chunks = [];
  let cursor = 0;
  let size = 12;
  while (cursor < text.length) {
    const end = Math.min(text.length, cursor + size);
    chunks.push(text.slice(cursor, end));
    cursor = end;
    // Deltas get longer as a reply streams, which is the shape that makes a
    // per-frame full rescan expensive.
    size = Math.min(target, size + 3);
  }
  return chunks;
}

function percentile(sorted, fraction) {
  if (sorted.length === 0) return 0;
  const index = Math.min(sorted.length - 1, Math.floor(sorted.length * fraction));
  return sorted[index];
}

function run(label, text, iterations = 30) {
  const chunks = chunksOf(text, 96);
  const frames = [];
  for (let round = 0; round < iterations; round += 1) {
    let source = "";
    let previous;
    for (const chunk of chunks) {
      source += chunk;
      const started = process.hrtime.bigint();
      previous = scanStreamingMarkdown(source, previous);
      const elapsedNs = Number(process.hrtime.bigint() - started);
      frames.push(elapsedNs / 1e6);
    }
  }
  frames.sort((a, b) => a - b);
  // §14.6: max is recorded, never used as a gate — GC makes a single slow frame
  // normal. The drop-frame count is the gate: two consecutive frames over 33 ms.
  let drops = 0;
  for (let index = 1; index < frames.length; index += 1) {
    if (frames[index] > 33 && frames[index - 1] > 33) drops += 1;
  }
  console.log(
    `${label}: frames=${frames.length} ` +
      `p50=${percentile(frames, 0.5).toFixed(3)}ms ` +
      `p99=${percentile(frames, 0.99).toFixed(3)}ms ` +
      `p99.9=${percentile(frames, 0.999).toFixed(3)}ms ` +
      `max=${frames[frames.length - 1].toFixed(3)}ms drops=${drops}`,
  );
}

const text = document();
console.log(`document: ${text.length} chars, ${chunksOf(text, 96).length} deltas`);
run("incremental", text);
// The same document scanned without handing the previous frame back, which is
// what the module's comment says the old implementation did every frame.
function coldFrames(text) {
  const chunks = chunksOf(text, 96);
  const frames = [];
  let source = "";
  for (const chunk of chunks) {
    source += chunk;
    const started = process.hrtime.bigint();
    scanStreamingMarkdown(source);
    frames.push(Number(process.hrtime.bigint() - started) / 1e6);
  }
  frames.sort((a, b) => a - b);
  console.log(
    `full-rescan: frames=${frames.length} ` +
      `p50=${percentile(frames, 0.5).toFixed(3)}ms ` +
      `p99=${percentile(frames, 0.99).toFixed(3)}ms ` +
      `max=${frames[frames.length - 1].toFixed(3)}ms`,
  );
}
coldFrames(text);
