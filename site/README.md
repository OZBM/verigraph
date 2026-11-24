# The VeriGraph site

A detailed, pedagogical website (in French) that presents VeriGraph, all its components and how to use
it, and that is itself built with VeriGraph: every value, curve, diagram layout, image, sound and hash
on it is computed by verified VeriGraph nodes compiled to WebAssembly, and every fact about VeriGraph
(hashes, obligation counts, counterexamples, sizes) comes from a real run of the toolchain recorded at
build time. JavaScript keeps time, routes events, formats text and moves bytes into the DOM.

```sh
site/build.sh        # prove 7 modules, check copied nodes, build 15 roots to wasm32-wasip1, write wasm/*.json and data/*.json
node site/test.mjs   # each binary against vg-interp (vg run), bit for bit, plus PRE refusals and reference vectors
node site/test-diagram.mjs
python3 -m http.server 8765 --directory site   # then open http://127.0.0.1:8765
```

## Layout

- `index.html` and twelve chapters (`idee`, `graphe`, `vg-sym`, `types`, `preuves`, `contre-exemple`,
  `compilation`, `agent`, `mesh`, `omnilift`, `ce-site`, `utiliser`) plus `glossaire.html`. `_modele.html` is
  the chapter template.
- `vg/`: the site's VG-Sym. `weblib.vgs` (easings, cubic-bezier, sine, colour, d3-style scales, pie and
  arc, the aurora, the fractal's zoom), `diagram.vgs` (`@layout`, which lays out every diagram), `demos.vgs`
  (the chapters' lab nodes: a plotting grid, the repaired soft-knee over a grid, the audio test chord),
  `draft.vgs` (the faulty soft-knee draft, built with `--unverified` on purpose).
- `wasm/`: the binaries `vg build` wrote, and `proofs.json`, `builds.json`, `bench.json`.
- `build/<page>.sh` and `data/<page>.json`: each chapter's toolchain runs and their recorded output.
- `css/site.css` (the design system), `js/vg.js` (the WASI host), `js/page.js` (chrome, motion, components),
  `js/diagram.js` (draws `@layout`'s output), `js/pages/<page>.js`.

The roots also come from `examples/mandelbrot.vgs` (`@render`), `examples/sha256.vgs` (`@sha256_impl`),
`examples/audio_limiter.vgs` (`@limit_frame`, with its SHA-256 resolved from a Mesh in `target/site/mesh`)
and zlib's `crc32_z`, lifted from `tests/lift/zlib/crc32.c` by `vg lift`.
