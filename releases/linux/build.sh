#!/usr/bin/env bash
# Builds the v2c Linux installers (.deb and AppImage) from this repository's source.
# The installers are not stored in git; run this after cloning or pulling.
#
#   bash releases/linux/build.sh          # check dependencies, then build
#   bash releases/linux/build.sh --check  # only list missing dependencies
#
# Output: releases/linux/v2c_<version>_amd64.deb, .AppImage and SHA256SUMS.txt (older
# versions in this folder are deleted). Never runs sudo or apt: when something is missing
# it prints the command for you to run. The build takes several minutes and a few GB of RAM.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
build="$here/../../tools/build-linux.sh"

case "${1:-}" in
    --check) exec bash "$build" --check ;;
    "") ;;
    -h | --help)
        sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *)
        echo "unknown argument: $1 (use --check or no argument)" >&2
        exit 2
        ;;
esac

if ! bash "$build" --check; then
    echo
    echo "Install the missing dependencies above, then run this script again." >&2
    exit 1
fi
echo
bash "$build"

echo
echo "Done. Install with:"
echo "  cd \"$here\" && sha256sum -c --ignore-missing SHA256SUMS.txt"
echo "  sudo apt install ./v2c_*_amd64.deb"
echo "Then run 'ibus restart' (or log out and in) and add the \"v2c\" input source"
echo "(docs/linux-setup.md section 7.2, or 7.2.1 for the command line)."
