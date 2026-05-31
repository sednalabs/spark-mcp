#!/usr/bin/env bash
set -euo pipefail

if [[ "${OSTYPE:-}" != "linux-gnu"* ]]; then
  echo "This script is tuned for Linux builds of ONNX Runtime." >&2
  echo "For macOS/Windows, follow the upstream ONNX Runtime build docs." >&2
  exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ORT_DIR="${ORT_DIR:-${ROOT_DIR}/.tmp/onnxruntime}"
ORT_TAG="${ORT_TAG:-v1.23.2}"
ORT_BUILD_DIR_NAME="${ORT_BUILD_DIR_NAME:-build}"
BUILD_DIR="${BUILD_DIR:-${ORT_DIR}/${ORT_BUILD_DIR_NAME}/Release}"
ORT_NICE="${ORT_NICE:-15}"
ORT_BUILD_JOBS="${ORT_BUILD_JOBS:-}"
ORT_FORCE_GENERIC="${ORT_FORCE_GENERIC:-0}"

if [[ ! -d "${ORT_DIR}/.git" ]]; then
  git clone --depth 1 --branch "${ORT_TAG}" https://github.com/microsoft/onnxruntime "${ORT_DIR}"
else
  git -C "${ORT_DIR}" fetch --tags
  git -C "${ORT_DIR}" checkout "${ORT_TAG}"
fi

extra_defines=()
extra_cxx_flags=("-Wno-error=range-loop-construct")
if [[ "${ORT_DISABLE_AVX2:-}" == "1" ]]; then
  extra_cxx_flags+=("-mno-avx2" "-mno-fma")
  extra_defines+=("CMAKE_C_FLAGS=-mno-avx2 -mno-fma")
fi

if (( ${#extra_cxx_flags[@]} )); then
  extra_defines+=("CMAKE_CXX_FLAGS=${extra_cxx_flags[*]}")
fi

extra_defines+=("onnxruntime_BUILD_UNIT_TESTS=OFF")
if [[ "${ORT_FORCE_GENERIC}" == "1" ]]; then
  extra_defines+=("onnxruntime_FORCE_GENERIC_ALGORITHMS=ON")
fi

cd "${ORT_DIR}"

build_args=(--build_dir "${ORT_BUILD_DIR_NAME}" --config Release --build_shared_lib --parallel --skip_tests)
if (( ${#extra_defines[@]} )); then
  build_args+=(--cmake_extra_defines "${extra_defines[@]}")
fi

if [[ -n "${ORT_BUILD_JOBS}" ]]; then
  export CMAKE_BUILD_PARALLEL_LEVEL="${ORT_BUILD_JOBS}"
fi

nice -n "${ORT_NICE}" python3 tools/ci_build/build.py "${build_args[@]}"

if [[ -f "${BUILD_DIR}/libonnxruntime.so" ]]; then
  echo "Built: ${BUILD_DIR}/libonnxruntime.so"
  echo "Export ORT_DYLIB_PATH=${BUILD_DIR}/libonnxruntime.so"
else
  echo "Build finished, but libonnxruntime.so not found at ${BUILD_DIR}." >&2
  echo "Search under ${ORT_DIR}/build for the shared library and set ORT_DYLIB_PATH accordingly." >&2
  exit 1
fi
