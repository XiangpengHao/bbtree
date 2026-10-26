cmake_minimum_required(VERSION 3.16)
project(shoal CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE RelWithDebInfo)
endif()

add_library(shoal INTERFACE)
target_include_directories(shoal INTERFACE ${CMAKE_CURRENT_SOURCE_DIR}/src)

# Sanitizers for the tests: SHOAL_SANITIZE=address;undefined, or thread.
set(SHOAL_SANITIZE "" CACHE STRING "Sanitizers for tests and benches")

option(SHOAL_NATIVE "Build tests and benches for the host CPU" ON)

function(shoal_target_options target)
  target_link_libraries(${target} PRIVATE shoal)
  if(MSVC)
    # C4324: a structure padded for its alignment, as `CachePadded` means to be.
    # C4702: "unreachable code", which MSVC reports after inlining a call that
    # throws into its caller, as the hash maps' tests do on purpose.
    target_compile_options(${target} PRIVATE /W4 /WX /permissive- /Zc:__cplusplus /utf-8 /wd4324 /wd4702)
    target_compile_definitions(${target} PRIVATE _CRT_SECURE_NO_WARNINGS)
    # MSVC has no "native": pick an `/arch` in CMAKE_CXX_FLAGS instead.
    if(SHOAL_SANITIZE MATCHES "address")
      target_compile_options(${target} PRIVATE /fsanitize=address)
    endif()
    return()
  endif()
  target_compile_options(${target} PRIVATE -Wall -Wextra -Werror)
  if(SHOAL_NATIVE)
    if(CMAKE_SYSTEM_PROCESSOR MATCHES "arm64|aarch64")
      target_compile_options(${target} PRIVATE -mcpu=native)
    else()
      target_compile_options(${target} PRIVATE -march=native)
    endif()
  endif()
  if(SHOAL_SANITIZE)
    string(REPLACE ";" "," sanitizers "${SHOAL_SANITIZE}")
    target_compile_options(${target} PRIVATE -fsanitize=${sanitizers} -fno-omit-frame-pointer
                                             -fno-sanitize-recover=all)
    target_link_options(${target} PRIVATE -fsanitize=${sanitizers})
  endif()
endfunction()

enable_testing()

file(GLOB SHOAL_TESTS ${CMAKE_CURRENT_SOURCE_DIR}/tests/*_test.cpp)
foreach(test_source ${SHOAL_TESTS})
  get_filename_component(test_name ${test_source} NAME_WE)
  add_executable(${test_name} ${test_source})
  shoal_target_options(${test_name})
  target_compile_definitions(${test_name} PRIVATE SHOAL_DEBUG_CHECKS=1)
  add_test(NAME ${test_name} COMMAND ${test_name})
  set_tests_properties(${test_name} PROPERTIES ENVIRONMENT
                       "TSAN_OPTIONS=suppressions=${CMAKE_CURRENT_SOURCE_DIR}/tests/tsan.supp halt_on_error=1")
endforeach()

file(GLOB SHOAL_BENCHES ${CMAKE_CURRENT_SOURCE_DIR}/bench/*_bench.cpp)
foreach(bench_source ${SHOAL_BENCHES})
  get_filename_component(bench_name ${bench_source} NAME_WE)
  add_executable(${bench_name} ${bench_source})
  shoal_target_options(${bench_name})
  if(MSVC)
    target_compile_options(${bench_name} PRIVATE /O2)
  else()
    target_compile_options(${bench_name} PRIVATE -O3)
  endif()
endforeach()

# The race against other C++ maps (bench/race/), which the library's zero
# dependencies leave out by default: oneTBB, folly, Abseil and Boost.
option(SHOAL_RACE_BENCH "Build the race against oneTBB, folly, Abseil and Boost maps" OFF)
if(SHOAL_RACE_BENCH)
  find_package(TBB CONFIG REQUIRED)
  # folly links `gflags_shared`, which gflags' package defines.
  find_package(gflags CONFIG REQUIRED)
  find_package(folly CONFIG REQUIRED)
  find_package(absl CONFIG REQUIRED)
  find_package(Boost 1.83 CONFIG REQUIRED)
  # Homebrew's folly exports the SDK's C headers as an include directory,
  # which puts them before libc++'s own.
  get_target_property(folly_includes Folly::folly_deps INTERFACE_INCLUDE_DIRECTORIES)
  list(FILTER folly_includes EXCLUDE REGEX "\\.sdk/usr/include$")
  set_target_properties(Folly::folly_deps PROPERTIES INTERFACE_INCLUDE_DIRECTORIES "${folly_includes}")
  add_executable(race_bench ${CMAKE_CURRENT_SOURCE_DIR}/bench/race/race_bench.cpp)
  shoal_target_options(race_bench)
  target_compile_options(race_bench PRIVATE -O3)
  target_link_libraries(race_bench PRIVATE TBB::tbb Folly::folly absl::btree absl::flat_hash_map Boost::headers)
endif()
