# Rapport de passation — VeriGraph (pour reprise par Claude Code)

Date : 2026-11-23
Branche active et unique : `main` (synchronisée avec `origin/main`)

Ce document résume l'état exact du dépôt afin que Claude Code puisse reprendre immédiatement sans phase de redécouverte.

---

## 1. État Git, branches et worktrees

- **Branche unique** : `main` (toutes les autres branches locales et distantes ont été mergées sur `main` et supprimées).
  - `sprint2/w0`, `sprint2/w2-telemetry`, `sprint2/w5-decode`, `sprint2/w6-mesh`, `sprint2/w7-omnilift` : intégrées dans `main`, worktrees supprimés, branches locales supprimées.
  - `claude/gate-g0-fixes-916487` : mergée dans `main` (`107353b`), worktree `.claude/worktrees/gate-g0-fixes-916487` supprimé, branches locale et distante (`origin/claude/gate-g0-fixes-916487`) supprimées.
- **Worktree unique** : `/Users/omarbenmustapha/Documents/verigraph` (`main`).
- **Fichiers non suivis préexistants laissés intacts** (à ne pas supprimer ni commiter sans accord explicite du propriétaire) :
  - `.impeccable/`
  - `docs/.DS_Store`
  - `docs/brochure/`
  - `docs/paper/`
  - `reports/gate-g0-review.md`
  - `reports/gate-g1-review.md`

---

## 2. Environnement et toolchain installés sur cet hôte (macOS Apple Silicon)

Tous les outils épinglés dans `toolchain.lock` passent `./scripts/check-toolchain.sh` (**10/10 OK**) :

| Outil | Version | Emplacement local |
| --- | --- | --- |
| `rustc` | `1.90.0` | `rustup` (`rust-toolchain.toml`) |
| `clang` / `opt` | `23.1.6` | `/opt/homebrew/opt/llvm/bin/{clang,opt}` |
| `host-clang` | `apple-21.0.0` | `/usr/bin/clang` (avec cible `wasm32`) |
| `z3` | `5.4.1` | `/opt/homebrew/bin/z3` |
| `cvc5` | `1.4.2` | `/opt/homebrew/bin/cvc5` (installé depuis le binaire officiel macOS arm64) |
| `bitwuzla` | `0.11.0` | `/opt/homebrew/bin/bitwuzla` |
| `wasi-libc` / `wasi-runtimes` | `34` / `23.1.6` | Homebrew WASI sysroot |
| `zig` | `0.17.1` | `/opt/homebrew/bin/zig` |
| `wasmtime` | `51.0.0` | `/opt/homebrew/bin/wasmtime` |
| `alive-tv` (Alive2) | compilé depuis `AliveToolkit/alive2` | `/opt/homebrew/bin/alive-tv` (lié contre LLVM 23.1.6 & Z3 5.4.1) |

---

## 3. Travail accompli et vérifié depuis le dernier arrêt de Claude Code

### Commits clés sur `main`
- **`94e4508`** — `vg-decode: propagate literal const_u64 in allowed(); refresh lark-parity.tsv (G1, G5)`
  - Passage de la suite complète du workspace après l'intégration de `sprint2/w0` et correction des deux échecs révélés (`crates/vg-decode/src/prefix.rs`, `tests/lark-parity.tsv`).
- **`5a7ea7e`** — `toolchain: pin cvc5 1.4.2 in toolchain.lock and check-toolchain.sh (P0, G3)`
  - Épinglage de `cvc5 1.4.2` dans `toolchain.lock` et `scripts/check-toolchain.sh`.
- **`ace5f4e`** — `vg-smt: Tseitin-name compound op results in pure_defs and avoid O(N^2) scratch clone (G3, G8)`
  - Dans `crates/vg-smt/src/verify.rs` : nommage Tseitin des expressions SMT scalaires composées (`is_compound_smt`, `name_compound_symval`) dans `qb.pure_defs` (en préservant intacts les littéraux SMT `(_ ...)`, `((_ to_fp ...) ...)` et les collections), et `scratch` léger au lieu d'un clone O(N²) de `qb` à chaque op. Corrige l'OOM (`SIGKILL`) lors de la vérification `vg-smt` de graphes déroulés comme `@compress256` (64 rondes SHA-256).
- **`1910684`** — `vg-omnilift: C/Rust LLVM IR lifting, UB curing, Alive2 + 1e6 fuzzing and G8 spec-class test (G8)`
  - Reprise et achèvement de `crates/vg-omnilift/src/{ingest,llvm,analysis,lift,verify}.rs` et `tests/lift/`.
  - Support complet des paramètres `out` / écritures tableau (`mbedtls_sha256_compress` et `rust-sha2::compress256`), cure UB (`Result::Err`, garde `read_slice_byte`, bornage dynamique des `SLICE`/`INDEX`/`UPDATE` dans les nœuds auxiliaires).
  - Vérification d'équivalence (`verify.rs`) :
    1. **Alive2 borné** (`alive-tv --disable-undef-input --src-unroll=8 --tgt-unroll=8`) via l'adaptateur `docs/08` (`PRE -> llvm.assume`, `Result::Err -> unreachable`), en abaissant `lifted.vgsym` dans l'ordre d'émission tout en vérifiant l'identité canonique avec `lifted.nodes`.
    2. **Fuzzing différentiel natif** (`1_000_000` entrées par fonction, lien `orig.ll` + `vg_codegen::emit_llvm` + harnais C).
  - Test d'intégration `crates/vg-omnilift/tests/g8.rs` :
    - Lève et valide les 4 cibles G8 (`zlib/crc32.c::crc32_z`, `zlib/adler32.c::adler32_z`, `mbedtls/sha256_shim.c::mbedtls_sha256_compress`, `rust-sha2/lib.rs::compress256`).
    - Ingère `SPEC @sha256_compress` (`tests/lift/spec/sha256.vgs`) et les 4 graphes dans `vg-mesh` (porte d'ingestion à 6 contrôles + journal C2SP + provenance) et atteste les deux SHA-256 (C et Rust) dans une même classe de spécification avec `evidence: "bounded(8)"`.
    - Tests négatifs : une mutation (`ADD.wrap -> SUB.wrap`) échoue avec `VG-L0002` (Alive2) et `VG-L0003` (fuzzing différentiel).
  - Compatibilité CI : les tests nécessitant `alive-tv` / `opt` sont ignorés sur les runners dépourvus de ces binaires sauf si `VG_REQUIRE_ALIVE2=1` (les fonctions de bibliothèque restent strictes et ne sont jamais affaiblies).
- **`1ca84ce`** — `vg-cli: add vg lift command and vg_lift MCP tool (P8.7, G5, G8)`
  - Commande CLI `vg lift FILE FUNCTION [--lang c|rust] [--root DIR]` et 8ᵉ outil MCP `vg_lift` (schéma `docs/15`), testé dans `crates/vg-cli/tests/mcp.rs`.
- **`107353b`** — `W0: merge claude/gate-g0-fixes-916487 into main (G0)`
  - Fusion des correctifs de la revue G0 (`scripts/gate-g0.sh`, `scripts/test-gate-g0.sh`, `reports/gate-g0.md`, vérification du triple hôte dans `.github/workflows/ci.yml`).
- **`7caea5b`** — `W0: tick P8.1-P8.7 in ROADMAP and log verified handoff state in PROGRESS (G8)`
  - Cases `P8.1`–`P8.7` cochées dans `ROADMAP.md` (aucune gate marquée passée) et entrée ajoutée en append-only dans `PROGRESS.md`.
- **Correctif CI (.github/workflows/ci.yml)** ajouté avec ce rapport : suppression d'une entrée `linux-x86_64` dupliquée issue de la résolution du conflit de merge sur `107353b` (qui avait fait échouer le run CI `45240805822` en 0 s avant lancement des jobs).

### Preuves de tests locaux (exécutés sur `7caea5b`)
- `./scripts/check-toolchain.sh` → `0` (`10/10 OK`)
- `cargo fmt --all --check` → `0`
- `cargo clippy --workspace --all-targets --locked -- -D warnings` → `0`
- `VG_REQUIRE_ALIVE2=1 cargo test --workspace --locked` → `0` (**36 suites de tests + 11 doc-tests : 203 passed, 0 failed, 1 ignored [`g7_bench_100k`]**)
  - `vg-cli`: `differential` (54.08s), `guide_examples` (1.06s), `mcp` (0.01s), `mutation` (191.50s), `sha256` (33.62s), `verifier_corpus` (15.32s)
  - `vg-omnilift`: 11 tests unitaires (17.95s) + 2 tests `g8` (20.07s) avec Alive2 `unroll=8` et `1_000_000` entrées chacun
  - `vg-telemetry`: `seeded_g6` (35.42s), `telemetry_acceptance` (38.28s)
  - `vg-mesh`: 7 unit + 16 `g7` (`g7_bench_100k` ignoré par défaut)
  - `vg-codegen`, `vg-core`, `vg-decode`, `vg-interp`, `vg-rt`, `vg-smt`, `vg-store`, `vg-types` : 100 % verts
- `node --test editors/antigravity/extension/test/*.test.js` → `0` (`14/14 passed`)
- `cargo run -p vg-cli --locked -- lift tests/lift/zlib/crc32.c crc32_z --lang c` → `0` (`"ok": true`, `"bounded_check": "alive2"`, `"unroll": 8`, `"fuzz_cases": 1000000`, `"node_hash": "c34090fe45d1c39600f3b83306d3615d9248b33d859fbffa10c458fbc11aabd4"`)

---

## 4. Ce qu'il reste à faire (ordre recommandé pour Claude Code)

1. **Vérifier le run GitHub Actions CI** déclenché après le correctif de `.github/workflows/ci.yml` (`linux-x86_64` et `macos-arm64`).
2. **Multi-solveurs `vg-smt` (P3.3 / G3 / `docs/15`)** :
   - Ajouter les drivers de processus `cvc5` (`/opt/homebrew/bin/cvc5 --lang smt2 --incremental`) et `bitwuzla` (`/opt/homebrew/bin/bitwuzla --incremental`) dans `crates/vg-smt/src/solver.rs` et `verify.rs`.
   - Implémenter le routage et le mode `release` / double solveur (`evidence: "two-solvers"`, diagnostic `VG-V0009` en cas de désaccord).
   - Compléter P3.4 (recherche petit modèle + replay guidé par modèle), P3.5 (cache de requêtes par hash) et P3.6/P3.8 (cocher dans `ROADMAP.md` une fois vérifié).
3. **Finalisation Codegen & Runtime (P4.6, P4.7, P4.8 / G4)** :
   - **Manifestes sandbox (P4.6)** : génération dans `vg-codegen` / `vg build` (profil Seatbelt `.sb` / entitlements macOS, Landlock/seccomp Linux, preopens WASI dérivés des `Handle` et `EFFECTS`) + test prouvant le blocage d'un accès non autorisé.
   - **5 kernels de benchmark (P4.8)** dans `benchmarks/kernels/` face à `clang -O2 -ffp-contract=off` C (ratio médian ≤ 1.20, rapport sous `reports/bench/`).
   - **Différentiel G4 à 1 000 programmes (P4.7)** : exécuter `VG_DIFF_PROGRAMS=1000 VG_DIFF_INPUTS=100 cargo test -p vg-cli --test differential --locked` (natif + WASM sous `wasmtime`, avec et sans `--no-proof-facts`, et liaison `x86_64-unknown-linux-gnu`).
4. **Scripts de gates G2–G8, `Makefile` et rapports** :
   - Implémenter `scripts/gate-g2.sh` à `scripts/gate-g8.sh` (avec `VG_REQUIRE_ALIVE2=1` pour G8) et les cibles `make gate-g2`..`gate-g8` dans `Makefile`.
   - Produire `reports/gate-g2.md` à `reports/gate-g8.md` (ne jamais marquer une gate approuvée : validation réservée au propriétaire).
5. **Boucle hébergée de synthèse (P5.7 / G5 / M1.1)** — *soumis à l'accord du propriétaire car consomme des crédits API (`AGENTS.md` §10)* :
   - Écrire le harnais pilotant les 30 tâches de `benchmarks/synthesis/` (seuil ≥ 60 % vérifiées en ≤ 5 tours contre le contrat de référence, 0 acceptation non saine).
   - Exécuter la boucle sur `examples/audio_limiter.prompt.md` (M1.1), conserver le transcript, compiler les binaires 3 cibles avec manifestes sandbox et benchmarks contre la clause `BUDGET` (M1.2), puis rédiger `reports/m1.md` (M1.3).
6. **Post-M1** : `P9` (Alpha / M2), vagues de recherche `R-01`..`R-14`, `P10` (auto-hébergement / M3), `P11` (M4).
