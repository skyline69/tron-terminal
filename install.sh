#!/usr/bin/env bash
# Installs, upgrades or removes tron on Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/skyline69/tron-terminal/main/install.sh | bash
#   ./install.sh               from a checkout or an unpacked release archive
#   ./install.sh --uninstall   remove what an earlier run installed
#
# When a release has a prebuilt tron for this system, the script asks whether to
# install it or build from source. Written for bash 3.2, the version macOS ships.
# Run with --help for options.

set -euo pipefail

readonly REPO_SLUG="skyline69/tron-terminal"
readonly REPO_URL="https://github.com/$REPO_SLUG.git"
readonly APP_ID="dev.tron.Terminal"
readonly MIN_RUST="1.98"
# glibc of the AlmaLinux 9 container that builds the Linux release archives (release.yml).
readonly MIN_GLIBC="2.34"
# Where releases and sources are fetched from. The overrides serve mirrors and tests.
readonly API_URL=${TRON_INSTALL_API_URL:-https://api.github.com/repos/$REPO_SLUG}
readonly DOWNLOAD_URL=${TRON_INSTALL_DOWNLOAD_URL:-https://github.com/$REPO_SLUG/releases/download}
readonly RAW_URL=${TRON_INSTALL_RAW_URL:-https://raw.githubusercontent.com/$REPO_SLUG}

usage() {
	cat <<EOF
Usage: install.sh [options]

Installs tron. When a release has a prebuilt tron for this system, you choose
between installing it, which takes seconds, and building from source, which
needs Rust and takes a few minutes. Run from an unpacked release archive, the
script installs the binary in the archive. When tron is already installed, the
installed and new versions are compared and you are asked before anything
changes.

Options:
  -y, --yes         Answer yes to every question; choose the prebuilt release
                    unless the sources are newer
      --prebuilt    Install a prebuilt release without asking
      --build       Build from source without asking
      --system      Install for all users (/usr/local, needs sudo)
      --prefix DIR  Install under DIR instead of ~/.local or /usr/local
      --source DIR  Build the tron checkout in DIR
      --ref REF     Release tag (vX.Y.Z) or branch to install (default: latest release, or main)
      --uninstall   Remove tron as installed by this script
  -h, --help        Show this help

Linux: the binary goes to PREFIX/bin, and a desktop entry, icons, AppStream
metadata and shell completions go to PREFIX/share, so application launchers
and desktop search find tron.

macOS: tron.app goes to /Applications (or ~/Applications when that is not
writable), where Spotlight and Launchpad find it, and PREFIX/bin/tron links to it.
EOF
}

# ---------------------------------------------------------------------------
# Output and questions

if [ -t 2 ] && [ -z "${NO_COLOR:-}" ]; then
	bold=$'\033[1m' red=$'\033[31m' green=$'\033[32m' yellow=$'\033[33m' cyan=$'\033[36m' reset=$'\033[0m'
else
	bold="" red="" green="" yellow="" cyan="" reset=""
fi

info() { printf '%s==>%s %s\n' "$cyan" "$reset" "$*" >&2; }
success() { printf '%s==>%s %s\n' "$green" "$reset" "$*" >&2; }
warn() { printf '%swarning:%s %s\n' "$yellow" "$reset" "$*" >&2; }
die() {
	printf '%serror:%s %s\n' "$red" "$reset" "$*" >&2
	exit 1
}

# prompt TEXT HINT: shows a question and reads the answer from the terminal into
# $answer. Reading the terminal also works when the script is piped into bash.
prompt() {
	if ! { exec 3</dev/tty; } 2>/dev/null; then
		die "cannot ask \"$1\" without a terminal; run again with --yes"
	fi
	printf '%s%s%s %s ' "$bold" "$1" "$reset" "$2" >&2
	answer=""
	IFS= read -r answer <&3 || true
	exec 3<&-
}

# ask QUESTION DEFAULT: succeeds when the answer is yes. DEFAULT is y or n.
ask() {
	local question=$1 default=$2 hint
	if [ "$assume_yes" = 1 ]; then
		return 0
	fi
	if [ "$default" = y ]; then hint="[Y/n]"; else hint="[y/N]"; fi
	prompt "$question" "$hint"
	case $answer in
	[Yy]*) return 0 ;;
	[Nn]*) return 1 ;;
	"") [ "$default" = y ] ;;
	*) return 1 ;;
	esac
}

has() { command -v "$1" >/dev/null 2>&1; }

# Runs a command, through sudo when the install location needs it.
run() {
	if [ "$use_sudo" = 1 ]; then
		sudo "$@"
	else
		"$@"
	fi
}

# fetch URL [FILE]: downloads URL to FILE, or to standard output.
fetch() {
	if has curl; then
		curl -fsSL --connect-timeout 10 --retry 2 -o "${2:--}" "$1"
	elif has wget; then
		wget -q --timeout=10 -O "${2:--}" "$1"
	else
		return 1
	fi
}

# version_cmp A B prints -1, 0 or 1. Compares dot separated numbers and
# ignores pre-release and build suffixes.
version_cmp() {
	awk -v a="$1" -v b="$2" 'BEGIN {
		sub(/[-+].*/, "", a); sub(/[-+].*/, "", b)
		na = split(a, x, "."); nb = split(b, y, ".")
		n = na > nb ? na : nb
		for (i = 1; i <= n; i++) {
			if (x[i] + 0 < y[i] + 0) { print -1; exit }
			if (x[i] + 0 > y[i] + 0) { print 1; exit }
		}
		print 0
	}'
}

# ---------------------------------------------------------------------------
# Options and locations

assume_yes=0
system=0
action=install
method=""
prefix=""
source_dir=""
ref=""

while [ $# -gt 0 ]; do
	case $1 in
	-y | --yes) assume_yes=1 ;;
	--prebuilt) method=prebuilt ;;
	--build) method=build ;;
	--system) system=1 ;;
	--prefix)
		[ $# -ge 2 ] || die "--prefix needs a directory"
		prefix=$2
		shift
		;;
	--prefix=*) prefix=${1#*=} ;;
	--source)
		[ $# -ge 2 ] || die "--source needs a directory"
		source_dir=$2
		shift
		;;
	--source=*) source_dir=${1#*=} ;;
	--ref)
		[ $# -ge 2 ] || die "--ref needs a tag or branch"
		ref=$2
		shift
		;;
	--ref=*) ref=${1#*=} ;;
	--uninstall) action=uninstall ;;
	-h | --help)
		usage
		exit 0
		;;
	*) die "unknown option: $1 (see --help)" ;;
	esac
	shift
done

case $(uname -s) in
Linux) os=linux ;;
Darwin) os=macos ;;
*) die "unsupported system: $(uname -s). tron runs on Linux and macOS." ;;
esac

if [ -z "$prefix" ]; then
	if [ "$system" = 1 ]; then prefix=/usr/local; else prefix=$HOME/.local; fi
fi
case $prefix in
/*) ;;
*) prefix=$PWD/$prefix ;;
esac
bin_dir=$prefix/bin
if [ "$system" = 0 ] && [ "$prefix" = "$HOME/.local" ]; then
	share_dir=${XDG_DATA_HOME:-$HOME/.local/share}
else
	share_dir=$prefix/share
fi
manifest_file=$share_dir/tron/install-manifest

app_dir=""
if [ "$os" = macos ]; then
	if [ "$system" = 1 ] || [ -w /Applications ]; then
		app_dir=/Applications
	else
		app_dir=$HOME/Applications
	fi
fi

# sudo only when a target cannot be written as this user.
use_sudo=0
if [ "$(id -u)" != 0 ]; then
	for target in "$prefix" ${app_dir:+"$app_dir"}; do
		existing=$target
		while [ ! -e "$existing" ]; do existing=$(dirname "$existing"); done
		if [ ! -w "$existing" ]; then use_sudo=1; fi
	done
fi
if [ "$use_sudo" = 1 ]; then
	has sudo || die "$prefix is not writable and sudo is not available"
	info "Installing to $prefix needs administrator rights; sudo will ask for your password."
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/tron-install.XXXXXX")
cleanup() { rm -rf "$work"; }
trap cleanup EXIT

# Set while choosing what to install.
src=""             # directory with the tron sources, or with an unpacked release archive
binary=""          # the tron binary to install
target_version=""  # version that will be installed
source_version=""  # version a build from source would install, when known
release_tag=""     # release to download, such as v0.1.0
release_archive="" # archive of that release for this system
release_reason=""  # why no prebuilt release can be installed

# ---------------------------------------------------------------------------
# The installed copy

# Path of the installed tron: the copy this script manages, then any tron on PATH.
installed_binary() {
	local candidate
	for candidate in \
		${app_dir:+"$app_dir/tron.app/Contents/MacOS/tron"} \
		"$bin_dir/tron" \
		"$(command -v tron 2>/dev/null || true)"; do
		if [ -n "$candidate" ] && [ -x "$candidate" ]; then
			printf '%s\n' "$candidate"
			return
		fi
	done
}

# Version reported by `tron --version` ("tron 0.1.0").
binary_version() {
	"$1" --version 2>/dev/null | awk 'NR == 1 { print $NF }'
}

# ---------------------------------------------------------------------------
# Prebuilt releases

# Release archive suffix for this machine, such as x86_64-linux.
platform_name() {
	local arch
	case $(uname -m) in
	x86_64 | amd64) arch=x86_64 ;;
	aarch64 | arm64) arch=aarch64 ;;
	*) return 1 ;;
	esac
	printf '%s-%s\n' "$arch" "$os"
}

# Whether the C library can run the prebuilt Linux binary.
glibc_supported() {
	local version
	version=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{ print $2 }')
	[ -n "$version" ] && [ "$(version_cmp "$version" "$MIN_GLIBC")" != -1 ]
}

# Looks for a release with an archive for this system. Sets release_tag and
# release_archive, or release_reason when there is none.
find_release() {
	local platform path json version
	if ! platform=$(platform_name); then
		release_reason="there is no prebuilt tron for $(uname -m)"
		return 1
	fi
	if [ "$os" = linux ] && ! glibc_supported; then
		release_reason="the prebuilt tron needs glibc $MIN_GLIBC or newer"
		return 1
	fi
	if ! has curl && ! has wget; then
		release_reason="downloading a release needs curl or wget"
		return 1
	fi
	case $ref in
	"") path=releases/latest ;;
	v[0-9]*) path=releases/tags/$ref ;;
	*)
		release_reason="$ref is a branch, not a release"
		return 1
		;;
	esac
	if ! json=$(fetch "$API_URL/$path" 2>/dev/null); then
		release_reason="no release was found${ref:+ for $ref}"
		return 1
	fi
	release_tag=$(printf '%s\n' "$json" | grep -o '"tag_name": *"[^"]*"' | head -n 1 | sed 's/.*"\([^"]*\)"$/\1/')
	version=${release_tag#v}
	release_archive=tron-$version-$platform.tar.gz
	if [ -z "$release_tag" ] || ! printf '%s\n' "$json" | grep -q "\"name\": *\"$release_archive\""; then
		release_reason="the release ${release_tag:-found} has no archive for $platform"
		return 1
	fi
	target_version=$version
}

# Downloads, verifies and unpacks the release archive into the work directory.
download_release() {
	local archive=$work/$release_archive expected actual=""
	info "Downloading $release_archive"
	fetch "$DOWNLOAD_URL/$release_tag/$release_archive" "$archive" || die "downloading $release_archive failed"
	fetch "$DOWNLOAD_URL/$release_tag/$release_archive.sha256" "$archive.sha256" ||
		die "downloading the checksum of $release_archive failed"
	expected=$(awk '{ print $1; exit }' "$archive.sha256")
	if has sha256sum; then
		actual=$(sha256sum "$archive" | awk '{ print $1 }')
	elif has shasum; then
		actual=$(shasum -a 256 "$archive" | awk '{ print $1 }')
	fi
	if [ -z "$actual" ]; then
		warn "neither sha256sum nor shasum is installed; the download is not verified"
	elif [ "$actual" != "$expected" ]; then
		die "the checksum of $release_archive does not match; the download is damaged or was altered"
	fi
	tar -xzf "$archive" -C "$work"
	src=$work/${release_archive%.tar.gz}
	is_archive "$src" || die "$release_archive does not contain a tron build"
}

# Checks that the prebuilt binary can run here.
check_prebuilt() {
	local missing version
	version=$(binary_version "$binary")
	if [ -z "$version" ]; then
		die "the prebuilt tron does not run on this system. Build from source with --build."
	fi
	if [ "$version" != "$target_version" ]; then
		warn "the archive holds tron $version, not $target_version"
		target_version=$version
	fi
	if [ "$os" = linux ]; then
		if has ldd; then
			missing=$(ldd "$binary" 2>/dev/null | awk '/not found/ { printf " %s", $1 }')
			[ -z "$missing" ] || die "the prebuilt tron needs missing libraries:$missing. Install fontconfig, or build from source with --build."
		fi
		has tic || warn "tic from ncurses is missing; tron needs it to install its terminfo entry."
	fi
}

# ---------------------------------------------------------------------------
# Source and build

script_dir() {
	local path=${BASH_SOURCE[0]:-}
	if [ -n "$path" ] && [ -f "$path" ]; then
		cd "$(dirname "$path")" && pwd
	fi
}

is_checkout() { [ -f "$1/Cargo.toml" ] && [ -f "$1/crates/tron/Cargo.toml" ]; }

# An unpacked release archive: the binary next to this script, nothing to build.
is_archive() { [ -x "$1/tron" ] && [ -f "$1/dist/$APP_ID.desktop" ] && [ ! -f "$1/Cargo.toml" ]; }

# Version in the [workspace.package] table of a Cargo.toml.
cargo_version() {
	awk -F'"' '
		/^\[workspace\.package\]/ { section = 1; next }
		/^\[/ { section = 0 }
		section && /^version[[:space:]]*=/ { print $2; exit }
	' "$1"
}

# Clones the repository when there is no checkout to build.
clone_source() {
	has git || die "git is needed to download the tron sources. Install git, or run this script from a tron checkout."
	info "Cloning $REPO_URL${ref:+ ($ref)}"
	git clone --quiet --depth 1 ${ref:+--branch "$ref"} "$REPO_URL" "$work/tron-terminal"
	src=$work/tron-terminal
	source_version=$(cargo_version "$src/Cargo.toml")
}

# Finds the checkout to build, or the version the clone would build.
locate_source() {
	local here
	here=$(script_dir)
	if [ -n "$source_dir" ]; then
		is_checkout "$source_dir" || die "$source_dir is not a tron checkout"
		src=$(cd "$source_dir" && pwd)
	elif [ -z "$ref" ] && [ -n "$here" ] && is_checkout "$here"; then
		src=$here
	fi
	if [ -n "$src" ]; then
		source_version=$(cargo_version "$src/Cargo.toml")
	elif fetch "$RAW_URL/${ref:-main}/Cargo.toml" "$work/Cargo.toml" 2>/dev/null; then
		source_version=$(cargo_version "$work/Cargo.toml")
	fi
}

check_rust() {
	local have
	if ! has cargo; then
		if [ -x "$HOME/.cargo/bin/cargo" ]; then
			PATH=$HOME/.cargo/bin:$PATH
		else
			die "Rust is not installed. Install it from https://rustup.rs, then run this script again."
		fi
	fi
	have=$(rustc --version | awk '{ print $2 }')
	if [ "$(version_cmp "$have" "$MIN_RUST")" = -1 ]; then
		if has rustup && ask "tron needs Rust $MIN_RUST or newer, found $have. Run rustup update stable?" y; then
			rustup update stable
		else
			die "tron needs Rust $MIN_RUST or newer, found $have"
		fi
	fi
}

# Offers to install the libraries the Linux build links against.
check_linux_libraries() {
	local missing="" packages="" manager="" library
	if has pkg-config; then
		for library in wayland-client xkbcommon fontconfig; do
			pkg-config --exists "$library" || missing="$missing $library"
		done
	else
		missing=" pkg-config"
	fi
	has cc || missing="$missing cc"
	has tic || missing="$missing tic"
	[ -n "$missing" ] || return 0

	if has dnf; then
		manager="dnf install -y"
		packages="gcc pkgconf-pkg-config wayland-devel libxkbcommon-devel fontconfig-devel ncurses"
	elif has apt-get; then
		manager="apt-get install -y"
		packages="build-essential pkg-config libwayland-dev libxkbcommon-dev libfontconfig-dev ncurses-bin"
	elif has pacman; then
		manager="pacman -S --needed --noconfirm"
		packages="base-devel pkgconf wayland libxkbcommon fontconfig ncurses"
	elif has zypper; then
		manager="zypper install -y"
		packages="gcc pkg-config wayland-devel libxkbcommon-devel fontconfig-devel ncurses-utils"
	else
		die "missing build dependencies:$missing. Install the development files for Wayland, xkbcommon and fontconfig, a C compiler and ncurses."
	fi
	warn "missing build dependencies:$missing"
	if ask "Install them with: sudo $manager $packages?" y; then
		# shellcheck disable=SC2086 # the command and package lists are split on purpose
		sudo $manager $packages
	else
		die "install the build dependencies with: sudo $manager $packages"
	fi
}

check_macos_tools() {
	if ! xcode-select -p >/dev/null 2>&1; then
		xcode-select --install >/dev/null 2>&1 || true
		die "the Xcode command line tools are needed. Finish their installation, then run this script again."
	fi
}

build() {
	info "Building tron $target_version, this takes a few minutes"
	(cd "$src" && cargo build --release --locked -p tron)
	binary=$src/target/release/tron
	[ -x "$binary" ] || die "the build did not produce $binary"
}

# ---------------------------------------------------------------------------
# Choosing between a prebuilt release and a build

# Asks which way to install when both are possible. Defaults to the prebuilt
# release, unless the sources to build are newer than it.
choose_method() {
	local default=p hint="[P/b]" build_label="build from source"
	if [ -n "$source_version" ]; then
		build_label="build $source_version from source"
		if [ "$(version_cmp "$source_version" "$target_version")" = 1 ]; then
			default=b
			hint="[p/B]"
		fi
	fi
	if [ "$assume_yes" = 1 ]; then
		answer=$default
	else
		info "tron $target_version is available prebuilt for $(platform_name)."
		while :; do
			prompt "Install the prebuilt tron $target_version (p), or $build_label (b)?" "$hint"
			case ${answer:-$default} in
			[Pp]*)
				answer=p
				break
				;;
			[Bb]*)
				answer=b
				break
				;;
			*) warn "answer p for prebuilt or b for build" ;;
			esac
		done
	fi
	case ${answer:-$default} in
	p) method=prebuilt ;;
	*) method=build ;;
	esac
}

# Settles src, method and target_version before anything is downloaded or built.
pick_method() {
	local here
	here=$(script_dir)
	if [ "$method" != build ] && [ -z "$source_dir" ] && [ -z "$ref" ] && [ -n "$here" ] && is_archive "$here"; then
		src=$here
		binary=$src/tron
		method=prebuilt
		target_version=$(binary_version "$binary")
		return
	fi

	locate_source
	if [ "$method" = build ]; then
		target_version=$source_version
		return
	fi
	if find_release; then
		if [ "$method" != prebuilt ]; then
			choose_method
		fi
	elif [ "$method" = prebuilt ]; then
		die "no prebuilt tron can be installed: $release_reason"
	else
		info "Building from source, as $release_reason."
		method=build
	fi
	if [ "$method" = build ]; then
		target_version=$source_version
	fi
}

# ---------------------------------------------------------------------------
# Installation

manifest=()

# install_file MODE SOURCE DEST: replaces DEST atomically, so a running tron keeps working.
install_file() {
	local mode=$1 from=$2 dest=$3
	run mkdir -p "$(dirname "$dest")"
	run cp "$from" "$dest.new"
	run chmod "$mode" "$dest.new"
	run mv -f "$dest.new" "$dest"
	manifest+=("$dest")
}

sed_escape() { printf '%s' "$1" | sed 's/[&|\\]/\\&/g'; }

install_icons() {
	local svg=$src/dist/$APP_ID.svg icons=$share_dir/icons/hicolor size
	install_file 644 "$svg" "$icons/scalable/apps/$APP_ID.svg"
	install_file 644 "$src/dist/$APP_ID.png" "$icons/256x256/apps/$APP_ID.png"
	# Fixed sizes for launchers that skip scalable icons.
	for size in 16 24 32 48 64 128 512; do
		if has rsvg-convert; then
			rsvg-convert -w "$size" -h "$size" "$svg" -o "$work/$size.png"
		elif has magick; then
			magick -background none -density 384 "$svg" -resize "${size}x$size" "$work/$size.png"
		else
			return 0
		fi
		install_file 644 "$work/$size.png" "$icons/${size}x$size/apps/$APP_ID.png"
	done
}

install_completions() {
	local fish_dir
	"$binary" completions bash >"$work/tron.bash"
	install_file 644 "$work/tron.bash" "$share_dir/bash-completion/completions/tron"
	if has zsh; then
		"$binary" completions zsh >"$work/_tron"
		install_file 644 "$work/_tron" "$share_dir/zsh/site-functions/_tron"
	fi
	if has fish; then
		if [ "$system" = 0 ] && [ "$prefix" = "$HOME/.local" ]; then
			fish_dir=${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions
		else
			fish_dir=$share_dir/fish/vendor_completions.d
		fi
		"$binary" completions fish >"$work/tron.fish"
		install_file 644 "$work/tron.fish" "$fish_dir/tron.fish"
	fi
}

# Lets menus, KRunner, GNOME search and software centers pick up the new files.
refresh_linux_caches() {
	if has update-desktop-database; then
		run update-desktop-database -q "$share_dir/applications" || true
	fi
	if has gtk-update-icon-cache; then
		run gtk-update-icon-cache -q -t -f "$share_dir/icons/hicolor" || true
	fi
	if has kbuildsycoca6; then
		kbuildsycoca6 >/dev/null 2>&1 || true
	elif has kbuildsycoca5; then
		kbuildsycoca5 >/dev/null 2>&1 || true
	fi
}

install_linux() {
	local exec_path=$bin_dir/tron
	install_file 755 "$binary" "$bin_dir/tron"

	# Launchers do not always have ~/.local/bin on PATH, so the entry names the binary directly.
	case $exec_path in
	*[[:space:]]*) exec_path="\"$exec_path\"" ;;
	esac
	sed -e "s|^Exec=tron|Exec=$(sed_escape "$exec_path")|" \
		-e "s|^TryExec=tron|TryExec=$(sed_escape "$bin_dir/tron")|" \
		"$src/dist/$APP_ID.desktop" >"$work/$APP_ID.desktop"
	install_file 644 "$work/$APP_ID.desktop" "$share_dir/applications/$APP_ID.desktop"
	install_file 644 "$src/dist/$APP_ID.metainfo.xml" "$share_dir/metainfo/$APP_ID.metainfo.xml"
	install_icons
	install_completions
	refresh_linux_caches
}

# Writes the app icon into a Resources directory: Icon.icon compiled by actool
# (Liquid Glass on macOS 26 and later) when Xcode is installed, otherwise the
# classic icon drawn on Apple's grid.
make_icons() {
	local resources=$1 set=$work/Icon.iconset size png=$src/dist/macos/icon-1024.png
	if [ -d "$src/dist/macos/Icon.icon" ] && xcrun --find actool >/dev/null 2>&1; then
		mkdir -p "$work/icon"
		if xcrun actool "$src/dist/macos/Icon.icon" --compile "$work/icon" \
			--output-partial-info-plist "$work/icon/partial.plist" \
			--app-icon Icon --include-all-app-icons --enable-on-demand-resources NO \
			--development-region en --target-device mac --platform macosx \
			--minimum-deployment-target 11.0 >/dev/null 2>&1 && [ -f "$work/icon/Assets.car" ]; then
			cp "$work/icon/Assets.car" "$work/icon/Icon.icns" "$resources/"
			return
		fi
	fi
	# Release archives carry only the Linux icons: fall back to those.
	[ -f "$png" ] || png=$src/dist/$APP_ID.png
	mkdir -p "$set"
	for size in 16 32 128 256 512; do
		sips -z "$size" "$size" "$png" --out "$set/icon_${size}x$size.png" >/dev/null
		sips -z $((size * 2)) $((size * 2)) "$png" --out "$set/icon_${size}x$size@2x.png" >/dev/null
	done
	iconutil -c icns "$set" -o "$resources/Icon.icns"
}

install_macos() {
	local app=$app_dir/tron.app staging=$work/tron.app lsregister
	lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
	mkdir -p "$staging/Contents/MacOS" "$staging/Contents/Resources"
	cp "$binary" "$staging/Contents/MacOS/tron"
	# A browser download marks the files as quarantined, and Gatekeeper then refuses the app.
	if has xattr; then xattr -cr "$staging" 2>/dev/null || true; fi
	make_icons "$staging/Contents/Resources"
	cat >"$staging/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key><string>en</string>
	<key>CFBundleDisplayName</key><string>tron</string>
	<key>CFBundleExecutable</key><string>tron</string>
	<key>CFBundleIconFile</key><string>Icon</string>
	<key>CFBundleIconName</key><string>Icon</string>
	<key>CFBundleIdentifier</key><string>$APP_ID</string>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>CFBundleName</key><string>tron</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>$target_version</string>
	<key>CFBundleVersion</key><string>$target_version</string>
	<key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
	<key>LSMinimumSystemVersion</key><string>11.0</string>
	<key>NSHighResolutionCapable</key><true/>
	<key>NSHumanReadableCopyright</key><string>MIT OR Apache-2.0</string>
</dict>
</plist>
EOF
	if has codesign; then
		codesign --force --sign - "$staging" >/dev/null 2>&1 || warn "ad-hoc signing tron.app failed"
	fi

	run mkdir -p "$app_dir"
	if [ -e "$app" ]; then run rm -rf "$app"; fi
	run ditto "$staging" "$app"
	manifest+=("$app")

	run mkdir -p "$bin_dir"
	run ln -sfn "$app/Contents/MacOS/tron" "$bin_dir/tron"
	manifest+=("$bin_dir/tron")
	install_completions

	# Register with Launch Services and Spotlight right away instead of at the next scan.
	if [ -x "$lsregister" ]; then "$lsregister" -f "$app" >/dev/null 2>&1 || true; fi
	if has mdimport; then mdimport "$app" >/dev/null 2>&1 || true; fi
}

write_manifest() {
	run mkdir -p "$(dirname "$manifest_file")"
	printf '%s\n' "${manifest[@]}" "$manifest_file" >"$work/manifest"
	run cp "$work/manifest" "$manifest_file"
}

# Tells the user what else to do so a new shell finds tron.
path_hints() {
	local found
	case ":$PATH:" in
	*":$bin_dir:"*) ;;
	*) warn "$bin_dir is not on your PATH. Add it in your shell configuration to run tron from a shell." ;;
	esac
	found=$(command -v tron 2>/dev/null || true)
	if [ -n "$found" ] && [ "$found" != "$bin_dir/tron" ]; then
		warn "$found comes before $bin_dir/tron on your PATH and runs instead."
	fi
	if has zsh && [ "$system" = 0 ]; then
		info "zsh completions are in $share_dir/zsh/site-functions; add that directory to fpath if completions are missing."
	fi
}

# Asks before replacing an installed tron. Exits when the user declines.
confirm_change() {
	local current="" current_version="" again="Rebuild and reinstall it?"
	[ "$method" = prebuilt ] && again="Reinstall it?"
	current=$(installed_binary)
	if [ -n "$current" ]; then
		current_version=$(binary_version "$current")
	fi
	if [ -n "$current_version" ]; then
		previous_version=$current_version
		case $(version_cmp "$current_version" "$target_version") in
		-1) ask "tron $current_version is installed at $current. Upgrade to $target_version?" y ;;
		0) ask "tron $target_version is already installed at $current. $again" n ;;
		*) ask "tron $current_version at $current is newer than $target_version. Downgrade?" n ;;
		esac || {
			info "Nothing changed."
			exit 0
		}
	elif [ -n "$current" ]; then
		ask "A tron without a readable version is installed at $current. Replace it with $target_version?" y ||
			{
				info "Nothing changed."
				exit 0
			}
	else
		info "Installing tron $target_version into $prefix"
	fi
}

do_install() {
	previous_version=""
	pick_method
	# Without a known version to compare, clone first and read it from the sources.
	if [ "$method" = build ] && [ -z "$target_version" ]; then
		[ -n "$src" ] || clone_source
		target_version=$source_version
	fi
	[ -n "$target_version" ] || die "cannot tell which version of tron would be installed"
	confirm_change

	if [ "$method" = prebuilt ]; then
		if [ -z "$src" ]; then
			download_release
			binary=$src/tron
		fi
		check_prebuilt
		info "Installing the prebuilt tron $target_version"
	else
		if [ -z "$src" ]; then
			clone_source
			target_version=$source_version
		fi
		check_rust
		if [ "$os" = linux ]; then check_linux_libraries; else check_macos_tools; fi
		build
	fi
	if [ "$os" = linux ]; then install_linux; else install_macos; fi
	write_manifest

	if [ -n "$previous_version" ] && [ "$previous_version" != "$target_version" ]; then
		success "tron upgraded from $previous_version to $target_version"
	else
		success "tron $target_version installed"
	fi
	if [ "$os" = linux ]; then
		info "Find it as \"tron\" in your application launcher, or run: tron"
	else
		info "Open tron from Spotlight or Launchpad ($app_dir/tron.app), or run: tron"
	fi
	path_hints
}

do_uninstall() {
	local path removed=0
	if [ ! -f "$manifest_file" ]; then
		die "no installation found in $prefix (looked for $manifest_file). Use --system or --prefix for other locations."
	fi
	ask "Remove tron from $prefix? Your configuration in ~/.config/tron stays." y || { info "Nothing changed."; exit 0; }
	while IFS= read -r path; do
		case $path in
		"") ;;
		*/tron.app) run rm -rf "$path" ;;
		*) run rm -f "$path" ;;
		esac
		removed=$((removed + 1))
	done <"$manifest_file"
	if [ "$os" = linux ]; then refresh_linux_caches; fi
	success "tron removed ($removed files)"
}

if [ "$action" = uninstall ]; then
	do_uninstall
else
	do_install
fi
