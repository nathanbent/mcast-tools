#!/usr/bin/env bash
# Build a release binary, tarball and .deb for one target.
#
#   packaging/build-release.sh [TARGET]
#
# TARGET is one of:
#   x86_64-unknown-linux-gnu        (amd64: PCs and servers)  [default]
#   aarch64-unknown-linux-gnu       (arm64: Raspberry Pi 3/4/5 on 64-bit OS)
#   armv7-unknown-linux-gnueabihf   (armhf: Pi 2/3/4/Zero 2 on 32-bit OS)
#
# Meant to run as root in an Ubuntu 22.04 container, which is what the
# release workflow does: building against its glibc (2.35) keeps the binaries
# runnable on Ubuntu 22.04+, Debian 12+ and Raspberry Pi OS bookworm and
# newer. To build locally with Docker:
#
#   docker run --rm -v "$PWD:/src" -w /src ubuntu:22.04 \
#       packaging/build-release.sh aarch64-unknown-linux-gnu
#
# Output goes to dist/: mcast-<version>-<target>.tar.gz, mcast_<version>_<arch>.deb
# and a SHA256SUMS-<target> file. Set SKIP_DEPS=1 to skip installing packages.

set -euo pipefail

TARGET="${1:-x86_64-unknown-linux-gnu}"
CARGO_DEB_VERSION="3.8.0"

case "$TARGET" in
x86_64-unknown-linux-gnu) ARCH=amd64 TRIPLE=x86_64-linux-gnu CROSS="" ;;
aarch64-unknown-linux-gnu) ARCH=arm64 TRIPLE=aarch64-linux-gnu CROSS="gcc-aarch64-linux-gnu g++-aarch64-linux-gnu" ;;
armv7-unknown-linux-gnueabihf) ARCH=armhf TRIPLE=arm-linux-gnueabihf CROSS="gcc-arm-linux-gnueabihf g++-arm-linux-gnueabihf" ;;
*)
    echo "unsupported target: $TARGET" >&2
    exit 1
    ;;
esac

cd "$(dirname "$0")/.."

if [ "${SKIP_DEPS:-}" != 1 ]; then
    export DEBIAN_FRONTEND=noninteractive
    if [ "$ARCH" != amd64 ]; then
        dpkg --add-architecture "$ARCH"
        # Ubuntu serves ARM packages from a separate archive.
        if grep -q '^ID=ubuntu' /etc/os-release && ! grep -q ubuntu-ports /etc/apt/sources.list; then
            # shellcheck disable=SC1091
            codename=$(. /etc/os-release && echo "$VERSION_CODENAME")
            sed -i 's/^deb \(http\)/deb [arch=amd64] \1/' /etc/apt/sources.list
            for suite in "$codename" "$codename-updates" "$codename-security"; do
                echo "deb [arch=$ARCH] http://ports.ubuntu.com/ubuntu-ports $suite main universe"
            done >>/etc/apt/sources.list
        fi
    fi
    apt-get update -qq
    # $CROSS is deliberately unquoted: it lists zero or more packages.
    # shellcheck disable=SC2086
    apt-get install -y -qq --no-install-recommends \
        build-essential pkg-config cmake curl ca-certificates git \
        "libasound2-dev:$ARCH" $CROSS >/dev/null
    if ! command -v cargo >/dev/null && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
        # (rustup-init must keep its name: it picks its behaviour from argv[0].)
        dir=$(mktemp -d)
        curl -sSf -o "$dir/rustup-init" "https://static.rust-lang.org/rustup/dist/$(uname -m)-unknown-linux-gnu/rustup-init"
        chmod +x "$dir/rustup-init"
        "$dir/rustup-init" -y --no-modify-path --profile minimal --default-toolchain stable >/dev/null
        rm -rf "$dir"
    fi
fi
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
rustup target add "$TARGET" >/dev/null
if ! cargo deb --version 2>/dev/null | grep -q "$CARGO_DEB_VERSION"; then
    cargo install cargo-deb --version "$CARGO_DEB_VERSION" --locked --quiet
fi

# Cross-compiling: point the linker, C compiler (for libopus) and pkg-config
# (for ALSA) at the target's toolchain and multiarch libraries.
if [ "$ARCH" != amd64 ]; then
    UPPER=$(echo "$TARGET" | tr 'a-z-' 'A-Z_')
    export "CARGO_TARGET_${UPPER}_LINKER=$TRIPLE-gcc"
    export "CC_${TARGET//-/_}=$TRIPLE-gcc"
    export "CXX_${TARGET//-/_}=$TRIPLE-g++"
    export "AR_${TARGET//-/_}=$TRIPLE-ar"
    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_LIBDIR="/usr/lib/$TRIPLE/pkgconfig:/usr/share/pkgconfig"
fi
# libopus has NEON code paths that don't build against Ubuntu's armhf default
# FPU (VFPv3-D16). Every Raspberry Pi with an armv7 CPU (Pi 2/3/4, Zero 2)
# has NEON, so enable it: the armhf build won't run on armv7 boards without it.
if [ "$TARGET" = armv7-unknown-linux-gnueabihf ]; then
    export CFLAGS_armv7_unknown_linux_gnueabihf="-mfpu=neon"
fi
# Always build libopus from the bundled source and link it in, so the only
# runtime dependencies are glibc and libasound2.
export LIBOPUS_STATIC=1 LIBOPUS_NO_PKG=1

cargo build --release --locked --target "$TARGET" -p mcast
cargo deb --no-build --no-strip --target "$TARGET" -p mcast --output dist/

VERSION=$(cargo metadata --no-deps --format-version 1 |
    sed -n 's/.*"name":"mcast","version":"\([^"]*\)".*/\1/p')
NAME="mcast-$VERSION-$TARGET"
STAGE="target/dist/$NAME"
rm -rf "$STAGE" && mkdir -p "$STAGE"
cp "target/$TARGET/release/mcast" README.md packaging/systemd/mcast@.service \
    packaging/mcast.conf.example "$STAGE/"
tar -C target/dist -czf "dist/$NAME.tar.gz" "$NAME"

cd dist
sha256sum "$NAME.tar.gz" "mcast_${VERSION}"*"_$ARCH.deb" >"SHA256SUMS-$TARGET"
cat "SHA256SUMS-$TARGET"
