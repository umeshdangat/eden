# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

if(WIN32)
  add_executable(
    edenfs
    win/service/main.cpp
    win/service/WinService.cpp
  )
else()
  add_executable(
    edenfs
    service/oss/main.cpp
  )
endif()

install(TARGETS edenfs RUNTIME DESTINATION bin)
target_include_directories(
  edenfs
  PUBLIC
    ${CMAKE_SOURCE_DIR}
    ${CMAKE_BINARY_DIR}
)
target_link_libraries(
  edenfs
    eden_service
    Folly::folly
)

add_library(
  eden_build_config STATIC
  eden-config.cpp
)

if (WIN32)
  add_compile_definitions(_UNICODE UNICODE)
endif()

add_subdirectory(config)
add_subdirectory(fuse)
add_subdirectory(inodes)
add_subdirectory(journal)
add_subdirectory(model)
add_subdirectory(rocksdb)
add_subdirectory(service)
add_subdirectory(sqlite)
add_subdirectory(store)
add_subdirectory(telemetry)
add_subdirectory(utils)
add_subdirectory(testharness)
add_subdirectory(py)
add_subdirectory(cli)


# Platform specific code
if (WIN32)
  add_subdirectory(win)
else()
  add_subdirectory(notifications)
  add_subdirectory(takeover)
endif()
