#!/usr/bin/env bash
# scripts/sync_vst3_sdk.sh
# Extracts the minimal VST3 SDK subset needed to build shim.cpp into
# vendor/vst3sdk/. Re-run when upgrading the SDK version.

set -euo pipefail

VST3_VERSION="${VST3_VERSION:-v3.7.9_build_61}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
VENDOR_DIR="${REPO_ROOT}/vendor/vst3sdk"

if [[ -d "${VENDOR_DIR}" ]]; then
    echo "vendor/vst3sdk already exists. Delete it to re-vendor." >&2
    exit 1
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

echo "cloning VST3 SDK ${VST3_VERSION} (with submodules)..."
git clone --depth 1 --branch "${VST3_VERSION}" \
    --recurse-submodules --shallow-submodules \
    https://github.com/steinbergmedia/vst3sdk.git \
    "${TMP_DIR}/vst3sdk"

SRC="${TMP_DIR}/vst3sdk"

echo "copying minimal subset to vendor/vst3sdk/..."
mkdir -p "${VENDOR_DIR}"
cp -r "${SRC}/pluginterfaces" "${VENDOR_DIR}/"
mkdir -p "${VENDOR_DIR}/public.sdk"
cp -r "${SRC}/public.sdk/source" "${VENDOR_DIR}/public.sdk/"

# Licensing note: Steinberg requires that the license file be preserved.
cp "${SRC}/LICENSE.txt" "${VENDOR_DIR}/" 2>/dev/null || true
cp "${SRC}/pluginterfaces/LICENSE.txt" "${VENDOR_DIR}/pluginterfaces/" 2>/dev/null || true
cp "${SRC}/public.sdk/LICENSE.txt" "${VENDOR_DIR}/public.sdk/" 2>/dev/null || true

echo "done."
du -sh "${VENDOR_DIR}"
