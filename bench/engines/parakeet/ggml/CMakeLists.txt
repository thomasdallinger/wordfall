cmake_minimum_required(VERSION 3.16)
project(parakeet_bench_server C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(WHISPER_CPP_SRC "" CACHE PATH "Path to a whisper.cpp checkout that contains include/parakeet.h")
if(NOT EXISTS "${WHISPER_CPP_SRC}/include/parakeet.h")
    message(FATAL_ERROR "WHISPER_CPP_SRC='${WHISPER_CPP_SRC}' has no include/parakeet.h")
endif()

set(BUILD_SHARED_LIBS        OFF CACHE BOOL "" FORCE)
set(WHISPER_BUILD_EXAMPLES   OFF CACHE BOOL "" FORCE)
set(WHISPER_BUILD_TESTS      OFF CACHE BOOL "" FORCE)
set(WHISPER_BUILD_SERVER     OFF CACHE BOOL "" FORCE)
add_subdirectory("${WHISPER_CPP_SRC}" whisper.cpp EXCLUDE_FROM_ALL)

add_executable(parakeet-server server.cpp)
target_link_libraries(parakeet-server PRIVATE parakeet)
