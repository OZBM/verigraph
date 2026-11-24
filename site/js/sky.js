// The hero's sky, off the main thread: each message is a loop phase t in [0, 1], and each reply
// is the 320 x 180 RGBA frame @aurora computed for it, with the time the binary took.
import { load, encode } from "./vg.js";

const aurora = await load(new URL("../wasm/aurora.wasm", import.meta.url));

onmessage = async ({ data: t }) => {
  try {
    const start = performance.now();
    const px = await aurora(encode([["f32", t]]));
    postMessage({ px, ms: performance.now() - start }, [px.buffer]);
  } catch (e) {
    postMessage({ error: String(e) });
  }
};
postMessage("ready");
