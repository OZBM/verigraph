#!/bin/sh
# Génère site/data/compilation.json à partir d'exécutions réelles de vg build.
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/compilation}
mkdir -p "$WORK" "$WORK/build1" "$WORK/build2" "$WORK/erasure_ok" "$WORK/erasure_unv" "$WORK/guard_demo" "$WORK/writer" site/data

"$VG" build examples/quickstart.vgs @midpoint \
  --target aarch64-apple-darwin \
  --target x86_64-unknown-linux-gnu \
  --target wasm32-wasip1 \
  --emit-llvm --root "$WORK" -o "$WORK/build1" >"$WORK/midpoint_build1.json"

"$VG" build examples/quickstart.vgs @midpoint \
  --target aarch64-apple-darwin \
  --target x86_64-unknown-linux-gnu \
  --target wasm32-wasip1 \
  --root "$WORK" -o "$WORK/build2" >"$WORK/midpoint_build2.json"

jq -e '.ok == true and .verified == true' "$WORK/midpoint_build1.json" >/dev/null
jq -e '.ok == true and .verified == true' "$WORK/midpoint_build2.json" >/dev/null

# Extrait les fonctions @vgi_ (interne) et @vg_ (entrée vérifiée) de l'IR LLVM de @midpoint.
awk '/^define .* @vgi_/{flag=1} flag{print; if($0=="}"){flag=0}}' \
  "$WORK/build1/midpoint-aarch64-apple-darwin.ll" >"$WORK/midpoint_internal.ll"
awk '/^define .* @vg_/{flag=1} flag{print; if($0=="}"){flag=0}}' \
  "$WORK/build1/midpoint-aarch64-apple-darwin.ll" >"$WORK/midpoint_checked.ll"

# Comparaison vérifié vs --unverified sur le même corps (ADD.checked(x, 1:u8)).
"$VG" build tests/corpus/good/g01_add_bounded.vgs @f \
  --target aarch64-apple-darwin --emit-llvm --root "$WORK" -o "$WORK/erasure_ok" >"$WORK/erasure_ok.json"
"$VG" build tests/corpus/bad/b01_add_overflow.vgs @f \
  --target aarch64-apple-darwin --unverified --emit-llvm --root "$WORK" -o "$WORK/erasure_unv" >"$WORK/erasure_unv.json"

awk '/^define .* @vgi_/{flag=1} flag{print; if($0=="}"){flag=0}}' \
  "$WORK/erasure_ok/f-aarch64-apple-darwin.ll" >"$WORK/erasure_ok_vgi.ll"
awk '/^define .* @vgi_/{flag=1} flag{print; if($0=="}"){flag=0}}' \
  "$WORK/erasure_unv/f-aarch64-apple-darwin.ll" >"$WORK/erasure_unv_vgi.ll"

# Exemple avec une garde résiduelle (ASSERT_GUARD dynamique non prouvé vrai).
cat >"$WORK/guard_demo.vgs" <<'EOF'
module guard_demo;
NODE @id_u32 {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {}
  BODY { RETURN(x); }
}
NODE @guard_main {
  SIGNATURE: (cond: Bool, x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = ASSERT_GUARD(cond, @id_u32, x, else=@id_u32);
    RETURN(%0);
  }
}
EOF
"$VG" build "$WORK/guard_demo.vgs" @guard_main \
  --target aarch64-apple-darwin --root "$WORK" -o "$WORK/guard_demo" >"$WORK/guard_demo.json"

# Exemple de manifeste avec poignée FileOut (identique au test sandbox.rs).
cat >"$WORK/writer.vgs" <<'EOF'
module writer;
NODE @main [intent: "Append stdin's bytes to the file its handle names", export] {
  SIGNATURE: (e: Eff, dst: FileOut, data: Slice<u8>) -> (Eff, Result<u64, u32>);
  EFFECTS: [FILE_WRITE];
  CONTRACT { }
  BODY {
    %0, %1 = FILE_WRITE(e, dst, data);
    RETURN(%0, %1);
  }
}
EOF
"$VG" build "$WORK/writer.vgs" @main \
  --target aarch64-apple-darwin --root "$WORK" -o "$WORK/writer" >"$WORK/writer_build.json"

jq -n -S \
  --slurpfile b1 "$WORK/midpoint_build1.json" \
  --slurpfile b2 "$WORK/midpoint_build2.json" \
  --rawfile ll_int "$WORK/midpoint_internal.ll" \
  --rawfile ll_chk "$WORK/midpoint_checked.ll" \
  --slurpfile eok "$WORK/erasure_ok.json" \
  --slurpfile eunv "$WORK/erasure_unv.json" \
  --slurpfile egd "$WORK/guard_demo.json" \
  --rawfile ll_ok "$WORK/erasure_ok_vgi.ll" \
  --rawfile ll_unv "$WORK/erasure_unv_vgi.ll" \
  --rawfile man_pure "$WORK/build1/midpoint.manifest.toml" \
  --rawfile man_writer "$WORK/writer/main.manifest.toml" \
  --rawfile b1_txt "$WORK/midpoint_build1.json" \
  --rawfile b2_txt "$WORK/midpoint_build2.json" \
  '{
    midpoint_build1: $b1[0],
    midpoint_build2: $b2[0],
    reproducibility: [
      range(0; ($b1[0].artifacts | length)) as $i |
      {
        target: $b1[0].artifacts[$i].target,
        bytes: $b1[0].artifacts[$i].bytes,
        blake3_build1: $b1[0].artifacts[$i].blake3,
        blake3_build2: $b2[0].artifacts[$i].blake3,
        identical: ($b1[0].artifacts[$i].blake3 == $b2[0].artifacts[$i].blake3),
        linked: $b1[0].artifacts[$i].linked,
        residual_guards: $b1[0].artifacts[$i].residual_guards
      }
    ],
    midpoint_llvm: {
      internal: $ll_int,
      checked: $ll_chk
    },
    erasure: {
      verified: {
        ok: $eok[0].ok,
        verified: $eok[0].verified,
        bytes: $eok[0].artifacts[0].bytes,
        blake3: $eok[0].artifacts[0].blake3,
        residual_guards: $eok[0].artifacts[0].residual_guards,
        ir: $ll_ok
      },
      unverified: {
        ok: $eunv[0].ok,
        verified: $eunv[0].verified,
        bytes: $eunv[0].artifacts[0].bytes,
        blake3: $eunv[0].artifacts[0].blake3,
        residual_guards: $eunv[0].artifacts[0].residual_guards,
        ir: $ll_unv
      },
      with_plain_guard: {
        ok: $egd[0].ok,
        verified: $egd[0].verified,
        bytes: $egd[0].artifacts[0].bytes,
        residual_guards: $egd[0].artifacts[0].residual_guards
      }
    },
    manifests: {
      pure_midpoint: $man_pure,
      file_writer: $man_writer
    },
    terminal_session: [
      { cmd: "vg build examples/quickstart.vgs @midpoint --target aarch64-apple-darwin --target x86_64-unknown-linux-gnu --target wasm32-wasip1 --emit-llvm -o target/site/compilation/build1", out: $b1_txt },
      { cmd: "vg build examples/quickstart.vgs @midpoint --target aarch64-apple-darwin --target x86_64-unknown-linux-gnu --target wasm32-wasip1 -o target/site/compilation/build2", out: $b2_txt }
    ]
  }' >site/data/compilation.json
