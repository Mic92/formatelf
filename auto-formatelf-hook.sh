# shellcheck shell=bash

# Thin setup hook around `auto-formatelf`. It only gathers library directories
# from the build inputs and forwards the configuration; all dependency
# resolution and patching lives in the Rust binary.

declare -a autoPatchelfLibs
declare -a extraAutoPatchelfLibs

gatherLibraries() {
    autoPatchelfLibs+=("$1/lib")
}

# shellcheck disable=SC2154
addEnvHooks "$targetOffset" gatherLibraries

# Register extra directories of shared objects for the next autoPatchelf run.
addAutoPatchelfSearchPath() {
    local -a findOpts=()
    while [ $# -gt 0 ]; do
        case "$1" in
            --) shift; break ;;
            --no-recurse) shift; findOpts+=("-maxdepth" 1) ;;
            --*) echo "addAutoPatchelfSearchPath: invalid argument: $1" >&2; return 1 ;;
            *) break ;;
        esac
    done
    local dir=
    while IFS= read -r -d '' dir; do
        extraAutoPatchelfLibs+=("$dir")
    done < <(find "$@" "${findOpts[@]}" \! -type d \
        \( -name '*.so' -o -name '*.so.*' \) -print0 | sed -z 's#/[^/]*$##' | uniq -z)
}

autoPatchelf() {
    local norecurse=
    while [ $# -gt 0 ]; do
        case "$1" in
            --) shift; break ;;
            --no-recurse) shift; norecurse=1 ;;
            --*) echo "autoPatchelf: invalid argument: $1" >&2; return 1 ;;
            *) break ;;
        esac
    done

    concatTo ignoreMissingDepsArray autoPatchelfIgnoreMissingDeps
    concatTo appendRunpathsArray appendRunpaths
    concatTo runtimeDependenciesArray runtimeDependencies
    concatTo autoPatchelfFlagsArray autoPatchelfFlags
    concatTo patchelfFlagsArray patchelfFlags

    auto-formatelf \
        ${norecurse:+--no-recurse} \
        --ignore-missing "${ignoreMissingDepsArray[@]}" \
        --paths "$@" \
        --libs "${autoPatchelfLibs[@]}" "${extraAutoPatchelfLibs[@]}" \
        --runtime-dependencies "${runtimeDependenciesArray[@]/%//lib}" \
        --append-rpaths "${appendRunpathsArray[@]}" \
        "${autoPatchelfFlagsArray[@]}" \
        --extra-args "${patchelfFlagsArray[@]}"
}

autoPatchelfPostFixup() {
    if [[ -z "${dontAutoPatchelf-}" ]]; then
        autoPatchelf -- $(for output in $(getAllOutputNames); do
            [ -e "${!output}" ] || continue
            [ "${output}" = debug ] && continue
            echo "${!output}"
        done)
    fi
}

postFixupHooks+=(autoPatchelfPostFixup)
