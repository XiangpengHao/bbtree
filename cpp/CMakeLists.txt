cmake_minimum_required(VERSION 3.16)
project(bbtree CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE RelWithDebInfo)
endif()

add_library(bbtree INTERFACE)
target_include_directories(bbtree INTERFACE ${CMAKE_CURRENT_SOURCE_DIR}/include)

# Sanitizers for the tests: BBTREE_SANITIZE=address;undefined, or thread.
set(BBTREE_SANITIZE "" CACHE STRING "Sanitizers for tests and benches")

option(BBTREE_NATIVE "Build tests and benches for the host CPU" ON)

function(bbtree_target_options target)
  target_link_libraries(${target} PRIVATE bbtree)
  if(MSVC)
    # C4324: a structure padded for its alignment, as `CachePadded` means to be.
    target_compile_options(${target} PRIVATE /W4 /WX /permissive- /Zc:__cplusplus /utf-8 /wd4324)
    target_compile_definitions(${target} PRIVATE _CRT_SECURE_NO_WARNINGS)
    # MSVC has no "native": pick an `/arch` in CMAKE_CXX_FLAGS instead.
    if(BBTREE_SANITIZE MATCHES "address")
      target_compile_options(${target} PRIVATE /fsanitize=address)
    endif()
    return()
  endif()
  target_compile_options(${target} PRIVATE -Wall -Wextra -Werror)
  if(BBTREE_NATIVE)
    if(CMAKE_SYSTEM_PROCESSOR MATCHES "arm64|aarch64")
      target_compile_options(${target} PRIVATE -mcpu=native)
    else()
      target_compile_options(${target} PRIVATE -march=native)
    endif()
  endif()
  if(BBTREE_SANITIZE)
    string(REPLACE ";" "," sanitizers "${BBTREE_SANITIZE}")
    target_compile_options(${target} PRIVATE -fsanitize=${sanitizers} -fno-omit-frame-pointer
                                             -fno-sanitize-recover=all)
    target_link_options(${target} PRIVATE -fsanitize=${sanitizers})
  endif()
endfunction()

enable_testing()

file(GLOB BBTREE_TESTS ${CMAKE_CURRENT_SOURCE_DIR}/tests/*_test.cpp)
foreach(test_source ${BBTREE_TESTS})
  get_filename_component(test_name ${test_source} NAME_WE)
  add_executable(${test_name} ${test_source})
  bbtree_target_options(${test_name})
  target_compile_definitions(${test_name} PRIVATE BBTREE_DEBUG_CHECKS=1)
  add_test(NAME ${test_name} COMMAND ${test_name})
  set_tests_properties(${test_name} PROPERTIES ENVIRONMENT
                       "TSAN_OPTIONS=suppressions=${CMAKE_CURRENT_SOURCE_DIR}/tests/tsan.supp halt_on_error=1")
endforeach()

file(GLOB BBTREE_BENCHES ${CMAKE_CURRENT_SOURCE_DIR}/bench/*_bench.cpp)
foreach(bench_source ${BBTREE_BENCHES})
  get_filename_component(bench_name ${bench_source} NAME_WE)
  add_executable(${bench_name} ${bench_source})
  bbtree_target_options(${bench_name})
  if(MSVC)
    target_compile_options(${bench_name} PRIVATE /O2)
  else()
    target_compile_options(${bench_name} PRIVATE -O3)
  endif()
endforeach()
