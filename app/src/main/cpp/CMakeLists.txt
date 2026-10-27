# SPDX-FileCopyrightText: 2026 HiFiPhile
# SPDX-License-Identifier: GPL-3.0-only

cmake_minimum_required(VERSION 3.22)
project(uvcamera C)

include(ExternalProject)

# libjpeg-turbo only supports being built as its own project.
set(LJT_DIR ${CMAKE_CURRENT_BINARY_DIR}/libjpeg-turbo)
ExternalProject_Add(libjpeg-turbo-build
    URL https://github.com/libjpeg-turbo/libjpeg-turbo/releases/download/3.1.2/libjpeg-turbo-3.1.2.tar.gz
    URL_HASH SHA256=8f0012234b464ce50890c490f18194f913a7b1f4e6a03d6644179fa0f867d0cf
    DOWNLOAD_EXTRACT_TIMESTAMP TRUE
    PREFIX ${LJT_DIR}
    INSTALL_DIR ${LJT_DIR}
    CMAKE_ARGS
        -DCMAKE_TOOLCHAIN_FILE=${CMAKE_TOOLCHAIN_FILE}
        -DCMAKE_MAKE_PROGRAM=${CMAKE_MAKE_PROGRAM}
        -DANDROID_ABI=${ANDROID_ABI}
        -DANDROID_PLATFORM=${ANDROID_PLATFORM}
        -DCMAKE_BUILD_TYPE=Release
        -DCMAKE_INSTALL_PREFIX=<INSTALL_DIR>
        -DCMAKE_INSTALL_LIBDIR=lib
        -DCMAKE_POSITION_INDEPENDENT_CODE=ON
        -DENABLE_SHARED=OFF
        -DWITH_TURBOJPEG=ON
        -DWITH_TOOLS=OFF
        -DWITH_TESTS=OFF
        -DREQUIRE_SIMD=ON
    BUILD_BYPRODUCTS ${LJT_DIR}/lib/libturbojpeg.a
)
file(MAKE_DIRECTORY ${LJT_DIR}/include)

add_library(turbojpeg STATIC IMPORTED)
add_dependencies(turbojpeg libjpeg-turbo-build)
set_target_properties(turbojpeg PROPERTIES
    IMPORTED_LOCATION ${LJT_DIR}/lib/libturbojpeg.a
    INTERFACE_INCLUDE_DIRECTORIES ${LJT_DIR}/include
)

add_library(uvcamera SHARED jpeg_encoder.c)
target_compile_options(uvcamera PRIVATE -O3 -Wall -Wextra -Werror)
target_link_libraries(uvcamera PRIVATE turbojpeg)
