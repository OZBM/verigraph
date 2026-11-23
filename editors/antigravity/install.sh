#!/bin/sh
# Installs VeriGraph into Antigravity IDE (docs/guide/antigravity.md): the vg compiler, the editor
# extension, and the agent plugin that serves `vg mcp` and the verigraph-synthesis skill.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
ide="${ANTIGRAVITY_IDE_CLI:-/Applications/Antigravity IDE.app/Contents/Resources/app/bin/antigravity-ide}"
# The IDE's agent is agy, which reads plugins from the global Antigravity configuration.
plugins="${ANTIGRAVITY_PLUGINS:-$HOME/.gemini/config/plugins}"
src="$repo/editors/antigravity"
out="$repo/target/antigravity"

echo "1/3 vg compiler"
cargo install --quiet --locked --path "$repo/crates/vg-cli" --target-dir "$repo/target"
vg="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin/vg"
"$vg" check "$repo/examples/quickstart.vgs" >/dev/null
echo "    $vg"

echo "2/3 editor extension"
version=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' "$src/extension/package.json")
stage="$out/stage"
rm -rf "$stage"
mkdir -p "$stage/extension/syntaxes"
for f in package.json extension.js lib.js language-configuration.json README.md syntaxes/vgsym.tmLanguage.json; do
  cp "$src/extension/$f" "$stage/extension/$f"
done
cat > "$stage/extension.vsixmanifest" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011" xmlns:d="http://schemas.microsoft.com/developer/vsx-schema-design/2011">
  <Metadata>
    <Identity Language="en-US" Id="verigraph" Version="$version" Publisher="verigraph"/>
    <DisplayName>VeriGraph</DisplayName>
    <Description xml:space="preserve">VG-Sym support for Antigravity IDE: highlighting, vg check diagnostics, and verify, run and build commands.</Description>
    <Categories>Programming Languages,Linters</Categories>
    <Properties>
      <Property Id="Microsoft.VisualStudio.Code.Engine" Value="^1.90.0"/>
      <Property Id="Microsoft.VisualStudio.Code.ExtensionKind" Value="workspace"/>
    </Properties>
  </Metadata>
  <Installation>
    <InstallationTarget Id="Microsoft.VisualStudio.Code"/>
  </Installation>
  <Dependencies/>
  <Assets>
    <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/>
    <Asset Type="Microsoft.VisualStudio.Services.Content.Details" Path="extension/README.md" Addressable="true"/>
  </Assets>
</PackageManifest>
EOF
cat > "$stage/[Content_Types].xml" <<'EOF'
<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension=".js" ContentType="application/javascript"/>
  <Default Extension=".json" ContentType="application/json"/>
  <Default Extension=".md" ContentType="text/markdown"/>
  <Default Extension=".vsixmanifest" ContentType="text/xml"/>
</Types>
EOF
# Fixed dates and a sorted entry list: the same sources give the same .vsix bytes.
find "$stage" -exec touch -t 198001010000 {} +
vsix="$out/verigraph-$version.vsix"
rm -f "$vsix"
(cd "$stage" && { echo '[Content_Types].xml'; echo extension.vsixmanifest; find extension -type f | LC_ALL=C sort; } | zip -X -q -nw "$vsix" -@)
"$ide" --install-extension "$vsix" --force
echo "    $vsix"

echo "3/3 agent plugin"
case "$vg" in *'"'* | *'\'*) echo "install.sh: cannot write $vg into JSON" >&2; exit 1 ;; esac
dest="$plugins/verigraph"
mkdir -p "$dest/skills"
cp "$src/plugin/plugin.json" "$dest/plugin.json"
rm -rf "$dest/skills/verigraph-synthesis"
cp -R "$repo/skills/verigraph-synthesis" "$dest/skills/verigraph-synthesis"
printf '{\n  "mcpServers": {\n    "verigraph": {\n      "command": "%s",\n      "args": ["mcp"]\n    }\n  }\n}\n' "$vg" > "$dest/mcp_config.json"
echo "    $dest"

echo "Done. Reload Antigravity IDE (Developer: Reload Window) to load the extension and the plugin."
