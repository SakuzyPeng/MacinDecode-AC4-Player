cmake_minimum_required(VERSION 3.24)
project(MacinDecodeMacinRender LANGUAGES C CXX)
include(FetchContent)
set(MR_ADM_CORE_BUILD_CLI OFF CACHE BOOL "" FORCE)
set(MR_ADM_CORE_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(MR_ADM_CORE_USE_INSTALLED_DEPS OFF CACHE BOOL "" FORCE)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(MR_ADM_BUILD_CAPI_BUNDLE OFF CACHE BOOL "" FORCE)
set(MR_ADM_BUILD_HEADMOTION OFF CACHE BOOL "" FORCE)
set(MR_ADM_ENABLE_SOFA ON CACHE BOOL "" FORCE)
set(MR_ADM_ENABLE_IAMF OFF CACHE BOOL "" FORCE)
set(MR_ADM_FLAC_PROVIDER VENDORED CACHE STRING "" FORCE)
set(MR_ADM_OPUS_PROVIDER VENDORED CACHE STRING "" FORCE)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
if(MSVC)
    # Match Rust's crt-static, including all transitive native targets.
    set(CMAKE_MSVC_RUNTIME_LIBRARY MultiThreaded CACHE STRING "" FORCE)
endif()
if(MACINRENDER_SOURCE_DIR)
    set(FETCHCONTENT_SOURCE_DIR_MACINRENDER "${MACINRENDER_SOURCE_DIR}")
else()
    unset(FETCHCONTENT_SOURCE_DIR_MACINRENDER CACHE)
    unset(FETCHCONTENT_SOURCE_DIR_MACINRENDER)
endif()
FetchContent_Declare(macinrender
    GIT_REPOSITORY https://github.com/SakuzyPeng/MacinRender-ADM-Core.git
    GIT_TAG a95eb043284c63b541d8439b1d98f65caa3c1b9a
    # Share downloaded sources, but isolate each Cargo build's native outputs.
    BINARY_DIR "${CMAKE_CURRENT_BINARY_DIR}/macinrender-build"
)
FetchContent_MakeAvailable(macinrender)

# An actual executable lets CMake resolve the static dependency closure and
# framework ordering. Cargo reads its File API link information, never Ninja
# command text. Building it also catches missing native symbols before rustc.
add_executable(macinrender_link_probe EXCLUDE_FROM_ALL link_probe.c)
set_target_properties(macinrender_link_probe PROPERTIES LINKER_LANGUAGE CXX)
target_link_libraries(macinrender_link_probe PRIVATE MacinRender::ADMCAPI)
if(APPLE)
    enable_language(OBJC)
    add_library(macindecode_headmotion STATIC "${macinrender_SOURCE_DIR}/gui/native/mr_headmotion.m")
    target_compile_options(macindecode_headmotion PRIVATE -fobjc-arc -fblocks)
    target_link_libraries(macindecode_headmotion PRIVATE "-framework CoreMotion" "-framework Foundation")
    target_link_libraries(macinrender_link_probe PRIVATE macindecode_headmotion)
    target_include_directories(macinrender_link_probe PRIVATE "${macinrender_SOURCE_DIR}/gui/native")
endif()
file(WRITE "${CMAKE_BINARY_DIR}/macinrender-source.txt" "${macinrender_SOURCE_DIR}")
file(WRITE "${CMAKE_BINARY_DIR}/macinrender-binary.txt" "${macinrender_BINARY_DIR}")
