// A C++ shared library built with a version script, so its .dynsym carries
// GNU symbol versions (VERDEF) and it pulls libstdc++. Exercises the
// versioning paths and --clear-symbol-version.
#include <string>

extern "C" int probe(int x) {
    std::string s(x, 'y');
    return static_cast<int>(s.size()) * 2 + 1;
}
