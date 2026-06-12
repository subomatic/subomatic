#!/usr/bin/env bash
# SIGNED release pipeline for the Subomatic desktop app (Tauri GUI). Runs locally
# or in CI via .github/workflows/release-signed.yml. Builds the macOS bundle,
# Developer-ID-signs + notarizes + staples the .app (via the Tauri bundler) and
# the .dmg (notarytool + stapler), minisigns the updater artifact, writes
# latest.json, and publishes everything to ONE public repo (subomatic/subomatic).
# The in-app updater fetches
#   https://github.com/subomatic/subomatic/releases/latest/download/latest.json
# anonymously, so the repo MUST stay public.
#
# The desktop app decodes audio in the webview (WebAudio) and runs the shared
# subomatic-core engine natively via Tauri commands — there is NO native libav
# here, so this build needs no ffmpeg/libclang (unlike the CLI).
#
# Usage: scripts/release.sh ["release notes…"]
# Bump the version in src-tauri/tauri.conf.json before releasing. Prereqs:
#   - gh logged in with push access to subomatic/subomatic
#   - ~/.tauri/subomatic-updater.key (minisign updater key; pubkey in tauri.conf.json)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Org/signing identifiers are NEVER hardcoded in this (public) repo. Locally they
# come from a gitignored scripts/release.env; in CI from repo Variables/Secrets
# exported as env. See scripts/release.env.example.
# shellcheck source=/dev/null
[ -f "${ROOT}/scripts/release.env" ] && . "${ROOT}/scripts/release.env"

RELEASES_REPO="subomatic/subomatic"
UPDATER_KEY="${HOME}/.tauri/subomatic-updater.key"
NOTES="${1:-Subomatic desktop release}"

VERSION="$(node -p "require('${ROOT}/src-tauri/tauri.conf.json').version")"
TAG="v${VERSION}"

[ -f "${UPDATER_KEY}" ] || { echo "error: updater key missing: ${UPDATER_KEY}" >&2; exit 1; }
# Abort if this version is already PUBLISHED as a full (non-prerelease) release.
if [ "$(gh release view "${TAG}" --repo "${RELEASES_REPO}" --json isPrerelease -q .isPrerelease 2>/dev/null)" = "false" ]; then
  echo "error: ${TAG} already published to ${RELEASES_REPO} — bump the version first" >&2
  exit 1
fi

# macOS signing identity (Developer ID) + notary creds come from the environment
# — CI from the repo Variable MACOS_SIGNING_IDENTITY + the API-key Secrets;
# locally from scripts/release.env. No identifiers are hardcoded. Notarization
# uses an App Store Connect API key (APPLE_API_KEY_PATH/_ISSUER/_KEY_ID) or an
# app-specific password (APPLE_ID/APPLE_PASSWORD); with neither, signed-not-notarized.
DEVID="${MACOS_SIGNING_IDENTITY:-}"
MAC_SIGN_ENV=()
NOTARIZE=0
# Sign whenever the identity NAME is configured — do NOT gate on `security
# find-identity`. Its -v filter does an online OCSP check that flakes on CI, and
# even without -v it depends on keychain search-list state; both intermittently
# dropped our (imported) cert and silently fell back to ad-hoc. CI imports the
# cert and makes its keychain the default; locally it's in the login keychain. A
# genuinely missing cert now fails codesign loudly instead of shipping ad-hoc.
if [ -n "${DEVID}" ]; then
  MAC_SIGN_ENV+=("APPLE_SIGNING_IDENTITY=${DEVID}")
  if [ -n "${APPLE_API_KEY_PATH:-}" ] && [ -n "${APPLE_API_ISSUER:-}" ] && [ -n "${APPLE_API_KEY_ID:-}" ]; then
    MAC_SIGN_ENV+=("APPLE_API_ISSUER=${APPLE_API_ISSUER}" "APPLE_API_KEY=${APPLE_API_KEY_ID}" "APPLE_API_KEY_PATH=${APPLE_API_KEY_PATH}")
    NOTARIZE=1
    echo ">> macOS: Developer ID sign + notarize (App Store Connect API key)"
  elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ]; then
    MAC_SIGN_ENV+=("APPLE_ID=${APPLE_ID}" "APPLE_PASSWORD=${APPLE_PASSWORD}" "APPLE_TEAM_ID=${APPLE_TEAM_ID}")
    NOTARIZE=1
    echo ">> macOS: Developer ID sign + notarize (app-specific password)"
  else
    echo ">> macOS: Developer ID sign only (no notary creds — a downloaded DMG will warn)"
  fi
else
  echo ">> WARNING: signing identity not in keychain — building AD-HOC (Gatekeeper will warn)" >&2
fi

echo ">> building macOS .app (Tauri signs + notarizes + staples it)"
# `env` is required: MAC_SIGN_ENV holds VAR=value strings, which bash only treats
# as assignments when literal at parse time (an array element would be run as a
# command). ${arr[@]+"${arr[@]}"} is bash-3.2-safe when the array is empty under
# `set -u` (the runner's bash).
(cd "${ROOT}" && env TAURI_SIGNING_PRIVATE_KEY="${UPDATER_KEY}" \
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
  ${MAC_SIGN_ENV[@]+"${MAC_SIGN_ENV[@]}"} npx tauri build --bundles app)

# subomatic's Cargo workspace is at the repo ROOT (src-tauri is a member), so
# tauri/cargo emit artifacts to <root>/target — NOT src-tauri/target.
BUNDLE_DIR="${ROOT}/target/release/bundle"
MAC_APP="${BUNDLE_DIR}/macos/Subomatic.app"
MAC_TARGZ="${BUNDLE_DIR}/macos/Subomatic.app.tar.gz"
[ -f "${MAC_TARGZ}.sig" ] || { echo "error: updater artifact sig missing — is createUpdaterArtifacts on?" >&2; exit 1; }
MAC_SIG="$(cat "${MAC_TARGZ}.sig")"

# Build the STYLED .dmg with appdmg: it writes the .DS_Store itself (background +
# icon layout + /Applications drag target), with NO AppleScript, so the styling
# survives headless CI — which Tauri's own dmg bundler does not. Then notarize +
# staple (notarytool accepts an unsigned dmg; the stapled ticket is what counts).
MAC_DMG="${BUNDLE_DIR}/macos/subomatic-${VERSION}-macos-arm64.dmg"
APPDMG_SPEC="$(mktemp -t subomatic-appdmg).json"
cat > "${APPDMG_SPEC}" <<JSON
{
  "title": "Subomatic",
  "background": "${ROOT}/src-tauri/dmg/background.png",
  "icon-size": 128,
  "window": { "size": { "width": 660, "height": 440 } },
  "contents": [
    { "x": 165, "y": 220, "type": "file", "path": "${MAC_APP}" },
    { "x": 495, "y": 220, "type": "link", "path": "/Applications" }
  ]
}
JSON
rm -f "${MAC_DMG}"
(cd "${ROOT}" && npx --yes appdmg "${APPDMG_SPEC}" "${MAC_DMG}")
rm -f "${APPDMG_SPEC}"
if [ "${NOTARIZE}" = "1" ]; then
  echo ">> signing + notarizing + stapling the DMG"
  # Sign the DMG itself (appdmg leaves it unsigned) so Gatekeeper has a usable
  # signature; then notarize + staple. Order: sign -> notarize -> staple.
  codesign --force --timestamp --sign "${DEVID}" "${MAC_DMG}"
  if [ -n "${APPLE_API_KEY_PATH:-}" ]; then
    xcrun notarytool submit "${MAC_DMG}" \
      --key "${APPLE_API_KEY_PATH}" --key-id "${APPLE_API_KEY_ID}" --issuer "${APPLE_API_ISSUER}" --wait
  else
    xcrun notarytool submit "${MAC_DMG}" \
      --apple-id "${APPLE_ID}" --password "${APPLE_PASSWORD}" --team-id "${APPLE_TEAM_ID}" --wait
  fi
  xcrun stapler staple "${MAC_DMG}"
fi

echo ">> generating latest.json"
LATEST="${ROOT}/target/latest.json"
NOTES_JSON="$(node -p 'JSON.stringify(process.argv[1])' "${NOTES}")"
cat > "${LATEST}" <<EOF
{
  "version": "${VERSION}",
  "notes": ${NOTES_JSON},
  "pub_date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "platforms": {
    "darwin-aarch64": {
      "signature": "${MAC_SIG}",
      "url": "https://github.com/${RELEASES_REPO}/releases/download/${TAG}/subomatic-macos-arm64.app.tar.gz"
    }
  }
}
EOF

# Asset names come from the file basename; stage renamed copies.
STAGE="$(mktemp -d)"
trap 'rm -rf "${STAGE}"' EXIT
cp "${MAC_DMG}" "${STAGE}/subomatic-${VERSION}-macos-arm64.dmg"
cp "${MAC_DMG}" "${STAGE}/subomatic-latest.dmg"   # stable, versionless link for salamacchine.it
cp "${MAC_TARGZ}" "${STAGE}/subomatic-macos-arm64.app.tar.gz"
cp "${LATEST}" "${STAGE}/latest.json"

REL_ASSETS=(
  "${STAGE}/subomatic-${VERSION}-macos-arm64.dmg"
  "${STAGE}/subomatic-latest.dmg"
  "${STAGE}/subomatic-macos-arm64.app.tar.gz"
  "${STAGE}/latest.json"
)
echo ">> publishing ${TAG} to ${RELEASES_REPO}"
# latest.json rides on the SAME release so /releases/latest/download/latest.json
# resolves to it. Must NOT be a prerelease — the updater's /releases/latest skips them.
if gh release view "${TAG}" --repo "${RELEASES_REPO}" >/dev/null 2>&1; then
  gh release upload "${TAG}" --repo "${RELEASES_REPO}" --clobber "${REL_ASSETS[@]}"
  gh release edit "${TAG}" --repo "${RELEASES_REPO}" --prerelease=false
else
  gh release create "${TAG}" --repo "${RELEASES_REPO}" \
    --title "Subomatic ${VERSION}" --notes "${NOTES}" \
    --target "$(git -C "${ROOT}" rev-parse HEAD)" "${REL_ASSETS[@]}"
fi

echo ">> done: https://github.com/${RELEASES_REPO}/releases/tag/${TAG}"
