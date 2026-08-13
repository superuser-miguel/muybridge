#!/usr/bin/env bash
# Publish Muybridge to its signed, auto-updating Flatpak repo.
#
# Muybridge ships two ways: a one-off .flatpak bundle on GitHub Releases, and
# this hosted OSTree repo at https://superuser-miguel.github.io/muybridge-repo/
# that `flatpak update` tracks. This script (re)builds the hosted repo and
# pushes it.
#
# Layout choice (deliberate, house rule): the published repo is regenerated
# wholesale and **force-pushed as a single commit** each release, so its git
# history never accumulates superseded, content-addressed OSTree objects. It is
# a separate GitHub repo from the code — the code repo stays clean.
#
# Prerequisites:
#   - flatpak-builder, ostree, git, gpg
#   - the signing secret key present in the local GPG keyring (see KEYID below);
#     losing it means you can no longer publish trusted updates to this remote.
#   - push access to git@github.com:superuser-miguel/muybridge-repo.git
#
# Usage:  build-aux/publish-repo.sh
set -euo pipefail

KEYID="D67DB8E03D50A8C0"          # signs the OSTree repo; the public key is baked
                                  # into the .flatpakref. NB: it signs the repo
                                  # only — git commits and tags are signed
                                  # separately, by the same key, via git config.
APP_ID="io.github.superuser_miguel.Muybridge"
PAGES_URL="https://superuser-miguel.github.io/muybridge-repo"
PUBLISH_REMOTE="git@github.com:superuser-miguel/muybridge-repo.git"
MANIFEST="io.github.superuser_miguel.Muybridge.release.yml"

# Must match `branch:` in the release manifest. The bundle on Releases is built
# from the same manifest, so both land on the same ref — install one over the
# other and it upgrades in place instead of leaving two Muybridges behind.
BRANCH="stable"

here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"

# Must live on the same filesystem as the flatpak-builder state dir, so NOT in
# /tmp — that is tmpfs here, which flatpak-builder rejects outright ("state dir
# is not on the same filesystem as the target dir") and which would put the
# whole cargo build in RAM anyway. Kept inside the project and gitignored.
work="$(mktemp -d "$here/.publish-tmp.XXXXXX")"
trap 'rm -rf "$work"' EXIT
repo="$work/repo"
build="$work/build-dir"

echo ">> Building the signed release into a fresh OSTree repo…"
flatpak-builder --user --force-clean --state-dir="$work/state" \
    --repo="$repo" --gpg-sign="$KEYID" "$build" "$MANIFEST"

echo ">> Generating static deltas + signing the summary…"
flatpak build-update-repo --generate-static-deltas --prune --gpg-sign="$KEYID" "$repo"

echo ">> Assembling the publish tree (repo + .flatpakref + landing page)…"
pub="$work/publish"
mkdir -p "$pub"
cp -a "$repo" "$pub/repo"
touch "$pub/.nojekyll"   # serve OSTree byte-for-byte; do not let Jekyll rewrite it

key_b64="$(gpg --export "$KEYID" | base64 --wrap=0)"
cat > "$pub/muybridge.flatpakref" <<EOF
[Flatpak Ref]
Name=${APP_ID}
Branch=${BRANCH}
Url=${PAGES_URL}/repo/
Title=Muybridge — pull still frames out of video
Homepage=https://superuser-miguel.github.io/muybridge/
Comment=Signed Flatpak repo for automatic updates
GPGKey=${key_b64}
RuntimeRepo=https://flathub.org/repo/flathub.flatpakrepo
IsRuntime=false
EOF

cat > "$pub/index.html" <<'EOF'
<!doctype html><meta charset=utf-8><title>Muybridge — Flatpak repo</title>
<style>body{font-family:system-ui,sans-serif;max-width:40rem;margin:4rem auto;padding:0 1rem;line-height:1.6}code{background:#f0f0f0;padding:.1em .3em;border-radius:3px}pre{background:#f0f0f0;padding:1rem;border-radius:6px;overflow-x:auto}@media (prefers-color-scheme:dark){body{background:#14171d;color:#e9edf3}code,pre{background:#1d222b}a{color:#62a0ea}}</style>
<h1>Muybridge — signed Flatpak repo</h1>
<p>Automatic updates for <a href="https://superuser-miguel.github.io/muybridge/">Muybridge</a>, which pulls still frames out of video.</p>
<pre><code>flatpak install --user https://superuser-miguel.github.io/muybridge-repo/muybridge.flatpakref
flatpak run io.github.superuser_miguel.Muybridge</code></pre>
<p>Updates then arrive with <code>flatpak update</code>. Signed with the project's GPG key.</p>
EOF

echo ">> Force-pushing as a single squashed commit…"
version="$(date +%Y-%m-%d)"
git -C "$pub" init -q -b main
git -C "$pub" add -A
git -C "$pub" -c user.name=superuser-miguel \
    -c user.email=16271056+superuser-miguel@users.noreply.github.com \
    commit -q -m "Publish Muybridge (${version}) — signed OSTree repo + .flatpakref"
git -C "$pub" remote add origin "$PUBLISH_REMOTE"
git -C "$pub" push -u --force origin main

echo ">> Done. Verify from the public URL:"
echo "   flatpak install --user ${PAGES_URL}/muybridge.flatpakref"
