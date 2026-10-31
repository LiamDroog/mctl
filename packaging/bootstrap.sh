#!/usr/bin/env bash
# Get a Linux machine ready for mctl and, from a source checkout, build and install it.
#
#   packaging/bootstrap.sh [options]
#
#   --check                 only report what is missing and change nothing (exit 1 if anything is)
#   --dry-run               show what would be done, without doing it
#   --yes                   do not ask before installing packages or Rust
#   --no-system-packages    never use the system package manager; print what is missing instead
#   --no-rust               do not install or update Rust
#   --no-build              get the machine ready, but do not build or install mctl
#   --prefix DIR            where to install mctl (default ~/.local; passed to install.sh)
#
# In a source checkout it makes sure there is a C compiler, the libraries the window needs at run time, a
# Vulkan driver and Rust at the minimum version (installing rustup if there is no suitable Rust), then runs
# install.sh, which builds and installs. In an unpacked release tarball, which already holds a built binary,
# it checks only the run-time pieces. Package names are known for apt (Debian, Ubuntu, Raspberry Pi OS, Mint),
# dnf (Fedora) and pacman (Arch); apt is the one that has been tried.
set -euo pipefail

check_only=0 dry=0 yes=0 use_pkg=1 use_rust=1 build=1 prefix=""
while [ $# -gt 0 ]; do
    case "$1" in
        --check) check_only=1; shift ;;
        --dry-run) dry=1; shift ;;
        --yes | -y) yes=1; shift ;;
        --no-system-packages) use_pkg=0; shift ;;
        --no-rust) use_rust=0; shift ;;
        --no-build) build=0; shift ;;
        --prefix) prefix="${2:?--prefix needs a directory}"; shift 2 ;;
        -h | --help) sed -n '2,18p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "bootstrap.sh: unknown option $1 (try --help)" >&2; exit 2 ;;
    esac
done

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(dirname "$here")"
source_tree=0
[ -f "$repo/Cargo.toml" ] && source_tree=1

if [ -t 1 ]; then g=$'\033[32m' r=$'\033[31m' y=$'\033[33m' n=$'\033[0m'; else g="" r="" y="" n=""; fi
say() { printf '%s\n' "$*"; }
die() { printf 'bootstrap.sh: %s\n' "$*" >&2; exit 1; }
good() { printf '  %sok%s       %s\n' "$g" "$n" "$*"; }
bad() { printf '  %smissing%s  %s\n' "$r" "$n" "$*"; }
old() { printf '  %stoo old%s  %s\n' "$y" "$n" "$*"; }

[ "$(uname -s)" = Linux ] || die "mctl runs on Linux only (this is $(uname -s))"
case "$(uname -m)" in
    x86_64 | aarch64) ;;
    *) say "note: $(uname -m) is not a platform mctl has been tried on" ;;
esac

# Which package manager, and so which package names.
pm=""
if command -v apt-get >/dev/null 2>&1; then pm=apt
elif command -v dnf >/dev/null 2>&1; then pm=dnf
elif command -v pacman >/dev/null 2>&1; then pm=pacman
fi

# What is needed: id | probe | apt package | dnf package | pacman package | what it is for
# A probe is `cmd:NAME`, `lib:SONAME` or `icd`.
needs=(
    "cc|cmd:cc|build-essential|gcc|gcc|a C compiler, for the dependencies' build scripts"
    "curl|cmd:curl|curl ca-certificates|curl|curl|curl, to fetch Rust"
    "vulkan|lib:libvulkan.so.1|libvulkan1|vulkan-loader|vulkan-icd-loader|the Vulkan loader"
    "icd|icd|mesa-vulkan-drivers|mesa-vulkan-drivers|vulkan-swrast|a Vulkan driver (your GPU's, or Mesa's software one)"
    "wayland|lib:libwayland-client.so.0|libwayland-client0|libwayland-client|wayland|the Wayland client library"
    "xkb|lib:libxkbcommon.so.0|libxkbcommon0|libxkbcommon|libxkbcommon|keyboard layouts"
    "xkbx|lib:libxkbcommon-x11.so.0|libxkbcommon-x11-0|libxkbcommon-x11|libxkbcommon-x11|keyboard layouts under X11"
    "x11|lib:libX11.so.6|libx11-6|libX11|libx11|X11"
    "xcursor|lib:libXcursor.so.1|libxcursor1|libXcursor|libxcursor|X11 cursors"
    "xi|lib:libXi.so.6|libxi6|libXi|libxi|X11 input"
    "xrandr|lib:libXrandr.so.2|libxrandr2|libXrandr|libxrandr|X11 display sizes"
    "xcb|lib:libxcb.so.1|libxcb1|libxcb|libxcb|X11 connections"
)

ldcache=""
if command -v ldconfig >/dev/null 2>&1; then ldcache="$(ldconfig -p 2>/dev/null || true)"
elif [ -x /sbin/ldconfig ]; then ldcache="$(/sbin/ldconfig -p 2>/dev/null || true)"; fi
have_lib() {
    if [ -n "$ldcache" ]; then grep -qF "$1" <<<"$ldcache"; return; fi
    compgen -G "/usr/lib*/$1" >/dev/null || compgen -G "/usr/lib/*/$1" >/dev/null || compgen -G "/lib*/$1" >/dev/null
}
have_icd() {
    [ -n "${VK_DRIVER_FILES:-}${VK_ICD_FILENAMES:-}" ] && return 0
    local d
    for d in /usr/share/vulkan/icd.d /etc/vulkan/icd.d /usr/local/share/vulkan/icd.d "${XDG_DATA_HOME:-$HOME/.local/share}/vulkan/icd.d"; do
        compgen -G "$d/*.json" >/dev/null && return 0
    done
    return 1
}

say "Checking this machine ($(uname -m), package manager: ${pm:-none found})"
missing_pkgs=()
problems=0
for row in "${needs[@]}"; do
    IFS='|' read -r id probe apt_pkg dnf_pkg pac_pkg what <<<"$row"
    # The compiler is only needed to build from source.
    if [ "$source_tree" = 0 ] && { [ "$id" = cc ] || [ "$id" = curl ]; }; then continue; fi
    case "$probe" in
        cmd:*)
            if command -v "${probe#cmd:}" >/dev/null 2>&1 || { [ "$id" = cc ] && command -v gcc >/dev/null 2>&1; }; then found=1; else found=0; fi
            ;;
        lib:*)
            if have_lib "${probe#lib:}"; then found=1; else found=0; fi
            ;;
        icd)
            if have_icd; then found=1; else found=0; fi
            ;;
    esac
    if [ "$found" = 1 ]; then
        good "$what"
    else
        bad "$what"
        problems=$((problems + 1))
        case "$pm" in
            # shellcheck disable=SC2206  # one row may name several packages: split on purpose
            apt) missing_pkgs+=($apt_pkg) ;;
            # shellcheck disable=SC2206
            dnf) missing_pkgs+=($dnf_pkg) ;;
            # shellcheck disable=SC2206
            pacman) missing_pkgs+=($pac_pkg) ;;
        esac
    fi
done

# Rust: needed only to build from source. Look on PATH and in the rustup default location.
msrv="1.95"
[ "$source_tree" = 1 ] && msrv="$(awk -F'"' '/^rust-version *=/ { print $2; exit }' "$repo/Cargo.toml")"
[ -n "$msrv" ] || msrv="1.95"
cargo_bin() { PATH="$PATH:$HOME/.cargo/bin" command -v "$1" 2>/dev/null || true; }
rust_state=skip rust_version=""
if [ "$source_tree" = 1 ]; then
    rustc_path="$(cargo_bin rustc)"
    if [ -z "$rustc_path" ] || [ -z "$(cargo_bin cargo)" ]; then
        rust_state=missing
        bad "Rust $msrv or newer (rustc and cargo)"
    else
        rust_version="$("$rustc_path" --version | awk '{print $2}')"
        if [ "$(printf '%s\n%s\n' "$msrv" "$rust_version" | sort -V | head -n1)" = "$msrv" ]; then
            rust_state=ok
            good "Rust $rust_version (needs $msrv or newer)"
        else
            rust_state=old
            old "Rust $rust_version (needs $msrv or newer)"
        fi
    fi
    [ "$rust_state" = ok ] || problems=$((problems + 1))
fi

if [ "$check_only" = 1 ]; then
    if [ "$problems" = 0 ]; then say "Everything mctl needs is here."; exit 0; fi
    say "$problems thing(s) missing. Run packaging/bootstrap.sh without --check to install them."
    exit 1
fi

run() { # print, and run unless this is a dry run
    [ "$check_only" = 0 ] || die "internal error: --check must not change anything (tried: $*)"
    printf '  $ %s\n' "$*"
    [ "$dry" = 1 ] || "$@"
}
confirm() {
    if [ "$yes" = 1 ] || [ "$dry" = 1 ]; then return 0; fi
    # A terminal that can really be opened, not just a /dev/tty file that exists.
    { : </dev/tty; } 2>/dev/null || die "needs your go-ahead but there is no terminal; run again with --yes"
    local a
    read -r -p "$1 [y/N] " a </dev/tty
    case "$a" in y | Y | yes | YES) return 0 ;; *) die "stopped; nothing more was changed" ;; esac
}

# System packages.
if [ "${#missing_pkgs[@]}" -gt 0 ]; then
    case "$pm" in
        apt) install_cmd=(apt-get install -y "${missing_pkgs[@]}"); refresh=(apt-get update) ;;
        dnf) install_cmd=(dnf install -y "${missing_pkgs[@]}"); refresh=() ;;
        pacman) install_cmd=(pacman -S --needed --noconfirm "${missing_pkgs[@]}"); refresh=() ;;
        *) die "no supported package manager (apt, dnf or pacman) found; install the missing items above by hand" ;;
    esac
    if [ "$use_pkg" = 0 ]; then
        say "Install these with your package manager, then run this again: ${missing_pkgs[*]}"
        exit 1
    fi
    sudo_cmd=()
    if [ "$(id -u)" != 0 ]; then
        command -v sudo >/dev/null 2>&1 || die "not root and no sudo; as root run: ${install_cmd[*]}"
        sudo_cmd=(sudo)
    fi
    say "Missing system packages: ${missing_pkgs[*]}"
    confirm "Install them with ${sudo_cmd[*]:-}${sudo_cmd[*]:+ }${install_cmd[0]}?"
    [ "${#refresh[@]}" -gt 0 ] && run "${sudo_cmd[@]}" "${refresh[@]}"
    run "${sudo_cmd[@]}" "${install_cmd[@]}"
fi

# Rust, through rustup (https://rustup.rs): the way the Rust project recommends, and the only one that
# reliably gives a recent enough compiler on a long-term-support distribution.
if [ "$source_tree" = 1 ] && [ "$rust_state" != ok ] && [ "$use_rust" = 1 ]; then
    if [ -n "$(cargo_bin rustup)" ]; then
        say "Updating Rust with rustup"
        confirm "Run rustup to install the stable toolchain?"
        run "$(cargo_bin rustup)" toolchain install stable --profile minimal
        run "$(cargo_bin rustup)" default stable
    else
        say "Installing Rust with the official rustup installer from https://sh.rustup.rs."
        say "It puts everything in ~/.cargo and ~/.rustup and adds ~/.cargo/bin to your shell profile."
        command -v curl >/dev/null 2>&1 || die "curl is needed to fetch rustup (install it, then run this again)"
        confirm "Install Rust now?"
        tmp="$(mktemp)"
        trap 'rm -f "$tmp"' EXIT
        run curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$tmp"
        run sh "$tmp" -y --profile minimal --default-toolchain stable
    fi
    if [ "$dry" = 0 ]; then
        # shellcheck source=/dev/null
        [ -r "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
        rustc_path="$(cargo_bin rustc)"
        [ -n "$rustc_path" ] || die "Rust did not install; see the output above"
        rust_version="$("$rustc_path" --version | awk '{print $2}')"
        [ "$(printf '%s\n%s\n' "$msrv" "$rust_version" | sort -V | head -n1)" = "$msrv" ] \
            || die "Rust $rust_version is still older than the $msrv mctl needs"
        good "Rust $rust_version"
    fi
elif [ "$source_tree" = 1 ] && [ "$rust_state" != ok ]; then
    die "Rust $msrv or newer is needed and --no-rust was given"
fi

[ "$build" = 1 ] || { say "Machine is ready. Build with: cargo build --release --locked -p mctl"; exit 0; }

# Building from source needs a good deal of memory for the linker.
if [ "$source_tree" = 1 ] && [ -r /proc/meminfo ]; then
    mem_mb=$(($(awk '/^MemTotal/ {print $2}' /proc/meminfo) / 1024))
    if [ "$mem_mb" -lt 2000 ]; then
        say "This machine has about ${mem_mb} MB of memory. Building mctl here is very slow and may run out of"
        say "memory; a prebuilt release (see the README) or building on another machine is a better idea."
        confirm "Build anyway?"
    fi
fi

args=()
[ -z "$prefix" ] || args+=(--prefix "$prefix")
say "Building and installing mctl$([ "$source_tree" = 1 ] && echo ' (a few minutes the first time)')"
if [ "$dry" = 1 ]; then
    printf '  $ %s%s\n' "$here/install.sh" "${args[*]:+ ${args[*]}}"
    exit 0
fi
"$here/install.sh" "${args[@]}"
bindir="${prefix:-$HOME/.local}/bin"
case ":$PATH:" in
    *":$bindir:"*) ;;
    *) say "Add $bindir to your PATH to run it as plain \`mctl\`." ;;
esac
say "Done. Try: mctl check   (fetches each data source once and reports)"
