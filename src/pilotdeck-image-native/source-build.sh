#!/bin/sh
set -eu
P=${PDRS_IMAGE_SOURCE_DIR:?source directory required}
S=$(mktemp -d "${OUT_DIR:?OUT_DIR required}/image-source-build.XXXXXXXX")
SCRIPT_DIR=${CARGO_MANIFEST_DIR:?manifest required}
mkdir "$S/src" "$S/prefix" "$S/build"
for archive in meson-1.9.1.tar.gz ninja-1.12.1.tar.gz zlib-ng-2.2.5.tar.gz libffi-3.5.2.tar.gz glib-2.86.1-git.tar.gz mozjpeg-0826579.tar.gz; do tar -xzf "$P/$archive" -C "$S/src"; done
tar -xJf "$P/vips-8.17.3.tar.xz" -C "$S/src"
tar -xJf "$P/expat-2.7.3.tar.xz" -C "$S/src"
tar -xJf "$P/libexif-0.6.25.tar.xz" -C "$S/src"
tar -xzf "$P/libspng-0.7.4.tar.gz" -C "$S/src"
tar -xzf "$P/libwebp-1.6.0.tar.gz" -C "$S/src"
tar -xzf "$P/gvdb-2b42fc75.tar.gz" --strip-components=1 -C "$S/src/glib-2.86.1/subprojects/gvdb"
tar -xjf "$P/pcre2-10.44.tar.bz2" -C "$S/src"
export CFLAGS='-O2 -fPIC' CXXFLAGS='-O2 -fPIC'
export PATH="$S/build/ninja:$S/prefix/bin:$PATH"
export PKG_CONFIG_PATH="$S/prefix/lib/pkgconfig:$S/prefix/share/pkgconfig"
export LD_LIBRARY_PATH="$S/prefix/lib"
MESON="$S/src/meson-1.9.1/meson.py"
printf '%s\n' 'PHASE ninja bootstrap'
cmake -S "$S/src/ninja-1.12.1" -B "$S/build/ninja" -DBUILD_TESTING=OFF -DFETCHCONTENT_FULLY_DISCONNECTED=ON -DCMAKE_BUILD_TYPE=Release
cmake --build "$S/build/ninja" --target ninja --parallel "${NUM_JOBS:-1}"
printf '%s\n' 'PHASE zlib-ng'
cmake -S "$S/src/zlib-ng-2.2.5" -B "$S/build/zlib" -DCMAKE_SKIP_INSTALL_RPATH=ON -DCMAKE_INSTALL_PREFIX="$S/prefix" -DCMAKE_INSTALL_LIBDIR=lib -DBUILD_SHARED_LIBS=ON -DZLIB_COMPAT=ON -DCMAKE_BUILD_TYPE=Release -DWITH_GTEST=OFF -DWITH_FUZZERS=OFF -DWITH_BENCHMARKS=OFF -DWITH_BENCHMARK_APPS=OFF -DZLIB_ENABLE_TESTS=OFF -DZLIBNG_ENABLE_TESTS=OFF -DFETCHCONTENT_FULLY_DISCONNECTED=ON
cmake --build "$S/build/zlib" --parallel "${NUM_JOBS:-1}"
cmake --install "$S/build/zlib"
printf '%s\n' 'PHASE libffi'
(cd "$S/build" && mkdir ffi && cd ffi && "$S/src/libffi-3.5.2/configure" --prefix="$S/prefix" --enable-shared --disable-docs --disable-multi-os-directory && make -j"${NUM_JOBS:-1}" && make install)
printf '%s\n' 'PHASE pcre2'
cmake -S "$S/src/pcre2-10.44" -B "$S/build/pcre2" -DCMAKE_SKIP_INSTALL_RPATH=ON -DCMAKE_INSTALL_PREFIX="$S/prefix" -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON -DBUILD_STATIC_LIBS=OFF -DPCRE2_BUILD_TESTS=OFF -DPCRE2_BUILD_PCRE2GREP=OFF -DPCRE2_SUPPORT_LIBREADLINE=OFF -DPCRE2_SUPPORT_LIBZ=OFF -DPCRE2_SUPPORT_LIBBZ2=OFF -DFETCHCONTENT_FULLY_DISCONNECTED=ON
cmake --build "$S/build/pcre2" --parallel "${NUM_JOBS:-1}"
cmake --install "$S/build/pcre2"
printf '%s\n' 'PHASE glib'
python3 "$MESON" setup "$S/build/glib" "$S/src/glib-2.86.1" --prefix="$S/prefix" --libdir=lib --buildtype=release --default-library=shared --wrap-mode=nodownload --force-fallback-for=gvdb -Dintrospection=disabled -Dnls=disabled -Dlibmount=disabled -Dsysprof=disabled -Dlibelf=disabled -Dtests=false -Dinstalled_tests=false -Dglib_assert=false -Dglib_checks=false -Dglib_debug=disabled
python3 "$MESON" compile -C "$S/build/glib" -j"${NUM_JOBS:-1}"
python3 "$MESON" install -C "$S/build/glib" --no-rebuild --tags bin-devel,devel,runtime
printf '%s\n' 'PHASE mozjpeg'
cmake -S "$S/src/mozjpeg-08265790774cd0714832c9e675522acbe5581437" -B "$S/build/mozjpeg" -DCMAKE_SKIP_INSTALL_RPATH=ON -DCMAKE_INSTALL_PREFIX="$S/prefix" -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_BUILD_TYPE=MinSizeRel -DBUILD_SHARED_LIBS=ON -DWITH_JPEG8=1 -DWITH_TURBOJPEG=OFF -DPNG_SUPPORTED=OFF -DWITH_SIMD=ON -DWITH_FUZZ=OFF -DFETCHCONTENT_FULLY_DISCONNECTED=ON
cmake --build "$S/build/mozjpeg" --target jpeg --parallel "${NUM_JOBS:-1}"
cmake --install "$S/build/mozjpeg" --component lib
cmake --install "$S/build/mozjpeg" --component include
printf '%s\n' 'PHASE expat'
cmake -S "$S/src/expat-2.7.3" -B "$S/build/expat" -DCMAKE_SKIP_INSTALL_RPATH=ON -DCMAKE_INSTALL_PREFIX="$S/prefix" -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_BUILD_TYPE=Release -DEXPAT_SHARED_LIBS=ON -DEXPAT_BUILD_TESTS=OFF -DEXPAT_BUILD_TOOLS=OFF -DEXPAT_BUILD_EXAMPLES=OFF -DEXPAT_BUILD_DOCS=OFF -DEXPAT_BUILD_FUZZERS=OFF -DEXPAT_OSSFUZZ_BUILD=OFF -DEXPAT_BUILD_PKGCONFIG=ON -DFETCHCONTENT_FULLY_DISCONNECTED=ON
cmake --build "$S/build/expat" --parallel "${NUM_JOBS:-1}"
cmake --install "$S/build/expat"
printf '%s\n' 'PHASE libexif'
(cd "$S/build" && mkdir libexif && cd libexif && "$S/src/libexif-0.6.25/configure" --prefix="$S/prefix" --libdir="$S/prefix/lib" --disable-nls --disable-docs && make -C libexif -j"${NUM_JOBS:-1}" && make -C libexif install && mkdir -p "$S/prefix/lib/pkgconfig" && install -m644 libexif.pc "$S/prefix/lib/pkgconfig/libexif.pc")
printf '%s\n' 'PHASE libspng'
python3 "$MESON" setup "$S/build/spng" "$S/src/libspng-fb768002d4288590083a476af628e51c3f1d47cd" --prefix="$S/prefix" --libdir=lib --buildtype=release --default-library=shared --wrap-mode=nodownload -Ddev_build=false -Dbuild_examples=false -Dbenchmarks=false -Doss_fuzz=false -Duse_miniz=false
python3 "$MESON" compile -C "$S/build/spng" -j"${NUM_JOBS:-1}"
python3 "$MESON" install -C "$S/build/spng" --no-rebuild
printf '%s\n' 'PHASE libwebp'
cmake -S "$S/src/libwebp-4fa21912338357f89e4fd51cf2368325b59e9bd9" -B "$S/build/webp" -DCMAKE_SKIP_INSTALL_RPATH=ON -DCMAKE_INSTALL_PREFIX="$S/prefix" -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON -DWEBP_BUILD_ANIM_UTILS=OFF -DWEBP_BUILD_CWEBP=OFF -DWEBP_BUILD_DWEBP=OFF -DWEBP_BUILD_GIF2WEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF -DWEBP_BUILD_VWEBP=OFF -DWEBP_BUILD_WEBPINFO=OFF -DWEBP_BUILD_WEBPMUX=OFF -DWEBP_BUILD_EXTRAS=OFF -DWEBP_BUILD_FUZZTEST=OFF -DFETCHCONTENT_FULLY_DISCONNECTED=ON
cmake --build "$S/build/webp" --parallel "${NUM_JOBS:-1}"
cmake --install "$S/build/webp"
printf '%s\n' 'PHASE libvips image codecs'
python3 "$SCRIPT_DIR/strip-vips-extras.py" "$S/src/vips-8.17.3"
python3 "$MESON" setup "$S/build/vips" "$S/src/vips-8.17.3" --prefix="$S/prefix" --libdir=lib --buildtype=release --default-library=shared --auto-features=disabled --wrap-mode=nodownload -Djpeg=enabled -Dexif=enabled -Dspng=enabled -Dwebp=enabled -Dnsgif=true -Dcplusplus=false -Dexamples=false -Ddocs=false -Ddeprecated=false -Dmodules=disabled -Dintrospection=disabled
python3 "$MESON" compile -C "$S/build/vips" -j"${NUM_JOBS:-1}"
python3 "$MESON" install -C "$S/build/vips" --no-rebuild
if [ "$(uname -s)" = Darwin ]; then
    python3 "$SCRIPT_DIR/fix-macos-install-names.py" "$S/prefix/lib"
fi
printf '%s\n' "$S/prefix" > "$OUT_DIR/.image-native-prefix.$$"
mv "$OUT_DIR/.image-native-prefix.$$" "$OUT_DIR/image-native-prefix"
printf '%s\n' 'SOURCE BUILD COMPLETE'
