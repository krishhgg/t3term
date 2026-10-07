const cells: number = 160 * 50;
const frames: number = Number(process.argv[2] ?? 30000);
const base = new Uint32Array(cells);
let previous = new Uint32Array(cells);
let next = new Uint32Array(cells);
for (let i = 0; i < cells; i++) base[i] = (32 + (i * 13 + Math.floor(i / 160) * 7) % 95) | ((Math.floor(i / 160) % 8) << 8);
let seed = 123456789;
let checksum = 2166136261;
let changed = 0;
const start = performance.now();
for (let frame = 0; frame < frames; frame++) {
  next.set(base);
  for (let k = 0; k < 8; k++) {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
    const pos = cells - 320 + seed % 320;
    next[pos] = (32 + ((seed >>> 16) % 95)) | (7 << 8);
  }
  for (let i = 0; i < cells; i++) {
    if (next[i] !== previous[i]) {
      changed++;
      checksum = Math.imul(checksum ^ next[i] ^ i, 16777619) >>> 0;
    }
  }
  [previous, next] = [next, previous];
}
console.log(JSON.stringify({frames, kernel_ms: performance.now()-start, checksum, changed}));
