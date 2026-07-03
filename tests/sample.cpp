// C++ pulls libstdc++ and libgcc_s (extra DT_NEEDED) and emits exception
// sections (.eh_frame, .gcc_except_table). Content is irrelevant.
#include <string>

int main() {
    std::string s(20, 'x');
    return static_cast<int>(s.size()) & 0;
}
